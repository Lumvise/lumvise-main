use crate::{
    AppCore, AppCoreError, CanvasPatch, CanvasSnapshot, DesktopBroadcastRecord, Result,
    ScreenshotRecord,
};
use lumvise_frontend_core::{
    FrontendRuntimeSnapshot, VoiceAudioChunk, VoicePlaybackChunk, VoicePlaybackSegment,
    VoicePlaybackStatus, VoiceRecording, WorkArea,
};
use lumvise_neural_core::llm_providers::{LlmRequest, LlmResponse};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use tokio::sync::{mpsc, oneshot};

mod workers;
use workers::{
    closed_frontend_handle, shutdown_frontend, shutdown_llms, shutdown_modalities,
    spawn_frontend_worker, spawn_llm_worker, spawn_modality_worker,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeWorkerState {
    Running,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSpawnMode {
    Daemon,
    App,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppCoreRuntimeHealth {
    pub frontend: RuntimeWorkerState,
    pub modalities: RuntimeWorkerState,
    pub llms: RuntimeWorkerState,
}

#[derive(Clone)]
pub struct AppCoreRuntime {
    frontend: RuntimeFrontendHandle,
    modalities: RuntimeModalityHandle,
    llms: RuntimeLlmHandle,
    state: Arc<RuntimeState>,
}

#[derive(Clone)]
pub struct RuntimeFrontendHandle {
    sender: mpsc::Sender<FrontendCommand>,
}

#[derive(Clone)]
pub struct RuntimeModalityHandle {
    sender: mpsc::Sender<ModalityCommand>,
}

#[derive(Clone)]
pub struct RuntimeLlmHandle {
    sender: mpsc::Sender<LlmCommand>,
}

struct RuntimeState {
    frontend: AtomicU8,
    modalities: AtomicU8,
    llms: AtomicU8,
}

enum FrontendCommand {
    SpawnApp(WorkArea, Response<FrontendRuntimeSnapshot>),
    UpdateCanvas(CanvasPatch, Response<CanvasSnapshot>),
    Canvas(Response<CanvasSnapshot>),
    Shutdown(oneshot::Sender<()>),
}

enum ModalityCommand {
    StartVoiceRecording {
        recording_id: String,
        media_type: String,
        response: Response<FrontendRuntimeSnapshot>,
    },
    StreamVoiceAudio {
        chunk: VoiceAudioChunk,
        response: Response<FrontendRuntimeSnapshot>,
    },
    CurrentVoiceRecording(Response<Option<VoiceRecording>>),
    OpenVoicePlayback {
        playback_id: String,
        response: Response<FrontendRuntimeSnapshot>,
    },
    AppendVoicePlaybackChunk {
        chunk: VoicePlaybackChunk,
        response: Response<FrontendRuntimeSnapshot>,
    },
    CloseVoicePlayback {
        playback_id: String,
        response: Response<FrontendRuntimeSnapshot>,
    },
    SetVoicePlaybackStatus {
        playback_id: String,
        status: VoicePlaybackStatus,
        response: Response<FrontendRuntimeSnapshot>,
    },
    CurrentVoicePlaybackSegment(Response<Option<VoicePlaybackSegment>>),
    RecordScreenshot {
        screenshot_id: String,
        media_type: String,
        bytes: Vec<u8>,
        response: Response<ScreenshotRecord>,
    },
    LatestScreenshot(Response<Option<ScreenshotRecord>>),
    RecordDesktopBroadcast {
        broadcast_id: String,
        event_kind: String,
        payload: Value,
        response: Response<DesktopBroadcastRecord>,
    },
    LatestDesktopBroadcast(Response<Option<DesktopBroadcastRecord>>),
    Shutdown(oneshot::Sender<()>),
}

enum LlmCommand {
    Complete {
        provider_id: String,
        request: LlmRequest,
        response: Response<LlmResponse>,
    },
    ProviderIds(Response<Vec<String>>),
    Shutdown(oneshot::Sender<()>),
}

type Response<T> = oneshot::Sender<Result<T>>;

impl AppCoreRuntime {
    /// Starts App Core domain workers over an injected App Core instance.
    ///
    /// # Example
    ///
    /// ```
    /// # let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
    /// # tokio_runtime.block_on(async {
    /// let runtime = lumvise_app_core::AppCoreRuntime::start(
    ///     lumvise_app_core::AppCore::in_memory().unwrap(),
    /// ).unwrap();
    /// assert_eq!(runtime.health().await.unwrap().frontend, lumvise_app_core::RuntimeWorkerState::Running);
    /// runtime.shutdown().await.unwrap();
    /// # });
    /// ```
    pub fn start(app: AppCore) -> Result<Self> {
        Self::start_app(app)
    }

    /// Starts App Core as a daemon without spawning the frontend worker.
    ///
    /// # Example
    ///
    /// ```
    /// # let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
    /// # tokio_runtime.block_on(async {
    /// let runtime = lumvise_app_core::AppCoreRuntime::start_daemon(
    ///     lumvise_app_core::AppCore::in_memory().unwrap(),
    /// ).unwrap();
    /// assert_eq!(runtime.health().await.unwrap().frontend, lumvise_app_core::RuntimeWorkerState::Stopped);
    /// runtime.shutdown().await.unwrap();
    /// # });
    /// ```
    pub fn start_daemon(app: AppCore) -> Result<Self> {
        Self::start_with_mode(app, RuntimeSpawnMode::Daemon)
    }

    /// Starts App Core in app mode with the frontend worker.
    ///
    /// # Example
    ///
    /// ```
    /// # let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
    /// # tokio_runtime.block_on(async {
    /// let runtime = lumvise_app_core::AppCoreRuntime::start_app(
    ///     lumvise_app_core::AppCore::in_memory().unwrap(),
    /// ).unwrap();
    /// assert_eq!(runtime.health().await.unwrap().frontend, lumvise_app_core::RuntimeWorkerState::Running);
    /// runtime.shutdown().await.unwrap();
    /// # });
    /// ```
    pub fn start_app(app: AppCore) -> Result<Self> {
        Self::start_with_mode(app, RuntimeSpawnMode::App)
    }

    /// Starts App Core with an explicit runtime spawn mode.
    ///
    /// # Example
    ///
    /// ```
    /// # let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
    /// # tokio_runtime.block_on(async {
    /// let runtime = lumvise_app_core::AppCoreRuntime::start_with_mode(
    ///     lumvise_app_core::AppCore::in_memory().unwrap(),
    ///     lumvise_app_core::RuntimeSpawnMode::Daemon,
    /// ).unwrap();
    /// runtime.shutdown().await.unwrap();
    /// # });
    /// ```
    pub fn start_with_mode(app: AppCore, mode: RuntimeSpawnMode) -> Result<Self> {
        let app = Arc::new(app);
        let state = Arc::new(RuntimeState::for_mode(mode));
        let frontend = frontend_handle_for_mode(mode, app.clone(), state.clone());
        let modalities = spawn_modality_worker(app.clone(), state.clone());
        let llms = spawn_llm_worker(app, state.clone());
        Ok(Self {
            frontend,
            modalities,
            llms,
            state,
        })
    }

    /// Returns the async frontend worker handle.
    ///
    /// # Example
    ///
    /// ```
    /// # let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
    /// # tokio_runtime.block_on(async {
    /// let runtime = lumvise_app_core::AppCoreRuntime::start(
    ///     lumvise_app_core::AppCore::in_memory().unwrap(),
    /// ).unwrap();
    /// let _frontend = runtime.frontend();
    /// runtime.shutdown().await.unwrap();
    /// # });
    /// ```
    pub fn frontend(&self) -> RuntimeFrontendHandle {
        self.frontend.clone()
    }

    /// Returns the async modality worker handle.
    ///
    /// # Example
    ///
    /// ```
    /// # let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
    /// # tokio_runtime.block_on(async {
    /// let runtime = lumvise_app_core::AppCoreRuntime::start(
    ///     lumvise_app_core::AppCore::in_memory().unwrap(),
    /// ).unwrap();
    /// let _modalities = runtime.modalities();
    /// runtime.shutdown().await.unwrap();
    /// # });
    /// ```
    pub fn modalities(&self) -> RuntimeModalityHandle {
        self.modalities.clone()
    }

    /// Returns the async LLM worker handle.
    ///
    /// # Example
    ///
    /// ```
    /// # let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
    /// # tokio_runtime.block_on(async {
    /// let runtime = lumvise_app_core::AppCoreRuntime::start(
    ///     lumvise_app_core::AppCore::in_memory().unwrap(),
    /// ).unwrap();
    /// let _llms = runtime.llms();
    /// runtime.shutdown().await.unwrap();
    /// # });
    /// ```
    pub fn llms(&self) -> RuntimeLlmHandle {
        self.llms.clone()
    }

    /// Reports current worker health from runtime state.
    ///
    /// # Example
    ///
    /// ```
    /// # let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
    /// # tokio_runtime.block_on(async {
    /// let runtime = lumvise_app_core::AppCoreRuntime::start(
    ///     lumvise_app_core::AppCore::in_memory().unwrap(),
    /// ).unwrap();
    /// assert_eq!(runtime.health().await.unwrap().llms, lumvise_app_core::RuntimeWorkerState::Running);
    /// runtime.shutdown().await.unwrap();
    /// # });
    /// ```
    pub async fn health(&self) -> Result<AppCoreRuntimeHealth> {
        Ok(self.state.health())
    }

    /// Stops all workers and returns final worker health.
    ///
    /// # Example
    ///
    /// ```
    /// # let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
    /// # tokio_runtime.block_on(async {
    /// let runtime = lumvise_app_core::AppCoreRuntime::start(
    ///     lumvise_app_core::AppCore::in_memory().unwrap(),
    /// ).unwrap();
    /// assert_eq!(runtime.shutdown().await.unwrap().frontend, lumvise_app_core::RuntimeWorkerState::Stopped);
    /// # });
    /// ```
    pub async fn shutdown(&self) -> Result<AppCoreRuntimeHealth> {
        shutdown_frontend(&self.frontend.sender).await;
        shutdown_modalities(&self.modalities.sender).await;
        shutdown_llms(&self.llms.sender).await;
        Ok(self.state.health())
    }
}

impl RuntimeFrontendHandle {
    /// Spawns the frontend app state through the frontend worker.
    ///
    /// # Example
    ///
    /// ```
    /// # let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
    /// # tokio_runtime.block_on(async {
    /// let runtime = lumvise_app_core::AppCoreRuntime::start(
    ///     lumvise_app_core::AppCore::in_memory().unwrap(),
    /// ).unwrap();
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// assert_eq!(runtime.frontend().spawn_app(area).await.unwrap().state.app.lifecycle, lumvise_frontend_core::AppLifecycle::Spawned);
    /// runtime.shutdown().await.unwrap();
    /// # });
    /// ```
    pub async fn spawn_app(&self, work_area: WorkArea) -> Result<FrontendRuntimeSnapshot> {
        request_response(
            "frontend",
            |response| FrontendCommand::SpawnApp(work_area, response),
            &self.sender,
        )
        .await
    }

    /// Updates the canvas through the frontend worker.
    ///
    /// # Example
    ///
    /// ```
    /// # let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
    /// # tokio_runtime.block_on(async {
    /// let runtime = lumvise_app_core::AppCoreRuntime::start(
    ///     lumvise_app_core::AppCore::in_memory().unwrap(),
    /// ).unwrap();
    /// let patch = lumvise_app_core::CanvasPatch { canvas_id: "main".into(), elements: vec![] };
    /// assert_eq!(runtime.frontend().update_canvas(patch).await.unwrap().revision, 1);
    /// runtime.shutdown().await.unwrap();
    /// # });
    /// ```
    pub async fn update_canvas(&self, patch: CanvasPatch) -> Result<CanvasSnapshot> {
        request_response(
            "frontend",
            |response| FrontendCommand::UpdateCanvas(patch, response),
            &self.sender,
        )
        .await
    }

    pub async fn canvas(&self) -> Result<CanvasSnapshot> {
        request_response("frontend", FrontendCommand::Canvas, &self.sender).await
    }
}

impl RuntimeModalityHandle {
    pub async fn start_voice_recording(
        &self,
        recording_id: &str,
        media_type: &str,
    ) -> Result<FrontendRuntimeSnapshot> {
        let command = |response| ModalityCommand::StartVoiceRecording {
            recording_id: recording_id.to_string(),
            media_type: media_type.to_string(),
            response,
        };
        request_response("modalities", command, &self.sender).await
    }

    pub async fn stream_voice_audio(
        &self,
        chunk: VoiceAudioChunk,
    ) -> Result<FrontendRuntimeSnapshot> {
        request_response(
            "modalities",
            |response| ModalityCommand::StreamVoiceAudio { chunk, response },
            &self.sender,
        )
        .await
    }

    pub async fn current_voice_recording(&self) -> Result<Option<VoiceRecording>> {
        request_response(
            "modalities",
            ModalityCommand::CurrentVoiceRecording,
            &self.sender,
        )
        .await
    }

    pub async fn open_voice_playback(&self, playback_id: &str) -> Result<FrontendRuntimeSnapshot> {
        let command = |response| ModalityCommand::OpenVoicePlayback {
            playback_id: playback_id.to_string(),
            response,
        };
        request_response("modalities", command, &self.sender).await
    }

    pub async fn append_voice_playback_chunk(
        &self,
        chunk: VoicePlaybackChunk,
    ) -> Result<FrontendRuntimeSnapshot> {
        request_response(
            "modalities",
            |response| ModalityCommand::AppendVoicePlaybackChunk { chunk, response },
            &self.sender,
        )
        .await
    }

    pub async fn close_voice_playback(&self, playback_id: &str) -> Result<FrontendRuntimeSnapshot> {
        let command = |response| ModalityCommand::CloseVoicePlayback {
            playback_id: playback_id.to_string(),
            response,
        };
        request_response("modalities", command, &self.sender).await
    }

    pub async fn set_voice_playback_status(
        &self,
        playback_id: &str,
        status: VoicePlaybackStatus,
    ) -> Result<FrontendRuntimeSnapshot> {
        let command = |response| ModalityCommand::SetVoicePlaybackStatus {
            playback_id: playback_id.to_string(),
            status,
            response,
        };
        request_response("modalities", command, &self.sender).await
    }

    pub async fn current_voice_playback_segment(&self) -> Result<Option<VoicePlaybackSegment>> {
        request_response(
            "modalities",
            ModalityCommand::CurrentVoicePlaybackSegment,
            &self.sender,
        )
        .await
    }

    pub async fn record_screenshot(
        &self,
        screenshot_id: &str,
        media_type: &str,
        bytes: Vec<u8>,
    ) -> Result<ScreenshotRecord> {
        let command = |response| ModalityCommand::RecordScreenshot {
            screenshot_id: screenshot_id.to_string(),
            media_type: media_type.to_string(),
            bytes,
            response,
        };
        request_response("modalities", command, &self.sender).await
    }

    pub async fn latest_screenshot(&self) -> Result<Option<ScreenshotRecord>> {
        request_response(
            "modalities",
            ModalityCommand::LatestScreenshot,
            &self.sender,
        )
        .await
    }

    pub async fn record_desktop_broadcast(
        &self,
        broadcast_id: &str,
        event_kind: &str,
        payload: Value,
    ) -> Result<DesktopBroadcastRecord> {
        let command = |response| ModalityCommand::RecordDesktopBroadcast {
            broadcast_id: broadcast_id.to_string(),
            event_kind: event_kind.to_string(),
            payload,
            response,
        };
        request_response("modalities", command, &self.sender).await
    }

    pub async fn latest_desktop_broadcast(&self) -> Result<Option<DesktopBroadcastRecord>> {
        request_response(
            "modalities",
            ModalityCommand::LatestDesktopBroadcast,
            &self.sender,
        )
        .await
    }
}

impl RuntimeLlmHandle {
    pub async fn complete(&self, provider_id: &str, request: LlmRequest) -> Result<LlmResponse> {
        let command = |response| LlmCommand::Complete {
            provider_id: provider_id.to_string(),
            request,
            response,
        };
        request_response("llms", command, &self.sender).await
    }

    pub async fn provider_ids(&self) -> Result<Vec<String>> {
        request_response("llms", LlmCommand::ProviderIds, &self.sender).await
    }
}

impl RuntimeState {
    fn for_mode(mode: RuntimeSpawnMode) -> Self {
        Self {
            frontend: AtomicU8::new(state_byte(frontend_state_for_mode(mode))),
            modalities: AtomicU8::new(state_byte(RuntimeWorkerState::Running)),
            llms: AtomicU8::new(state_byte(RuntimeWorkerState::Running)),
        }
    }

    fn health(&self) -> AppCoreRuntimeHealth {
        AppCoreRuntimeHealth {
            frontend: load_state(&self.frontend),
            modalities: load_state(&self.modalities),
            llms: load_state(&self.llms),
        }
    }
}

fn frontend_handle_for_mode(
    mode: RuntimeSpawnMode,
    app: Arc<AppCore>,
    state: Arc<RuntimeState>,
) -> RuntimeFrontendHandle {
    match mode {
        RuntimeSpawnMode::App => spawn_frontend_worker(app, state),
        RuntimeSpawnMode::Daemon => closed_frontend_handle(),
    }
}

fn frontend_state_for_mode(mode: RuntimeSpawnMode) -> RuntimeWorkerState {
    match mode {
        RuntimeSpawnMode::App => RuntimeWorkerState::Running,
        RuntimeSpawnMode::Daemon => RuntimeWorkerState::Stopped,
    }
}

async fn request_response<T, C, F>(
    worker: &str,
    build_command: F,
    sender: &mpsc::Sender<C>,
) -> Result<T>
where
    T: Send + 'static,
    C: Send + 'static,
    F: FnOnce(Response<T>) -> C,
{
    let (response, receiver) = oneshot::channel();
    sender
        .send(build_command(response))
        .await
        .map_err(|_| AppCoreError::worker_unavailable(worker))?;
    receiver
        .await
        .map_err(|_| AppCoreError::worker_unavailable(worker))?
}

fn stop_worker(state: &AtomicU8) {
    state.store(state_byte(RuntimeWorkerState::Stopped), Ordering::SeqCst);
}

fn load_state(state: &AtomicU8) -> RuntimeWorkerState {
    match state.load(Ordering::SeqCst) {
        1 => RuntimeWorkerState::Running,
        _ => RuntimeWorkerState::Stopped,
    }
}

fn state_byte(state: RuntimeWorkerState) -> u8 {
    match state {
        RuntimeWorkerState::Running => 1,
        RuntimeWorkerState::Stopped => 2,
    }
}
