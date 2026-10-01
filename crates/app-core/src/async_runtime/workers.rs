use super::{
    AppCore, DesktopBroadcastRecord, FrontendCommand, LlmCommand, ModalityCommand, Response,
    Result, RuntimeFrontendHandle, RuntimeLlmHandle, RuntimeModalityHandle, RuntimeState,
    ScreenshotRecord, stop_worker,
};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use tokio::sync::{mpsc, oneshot};

pub(super) fn spawn_frontend_worker(
    app: Arc<AppCore>,
    state: Arc<RuntimeState>,
) -> RuntimeFrontendHandle {
    let (sender, receiver) = mpsc::channel(64);
    tokio::spawn(frontend_worker(app, state, receiver));
    RuntimeFrontendHandle { sender }
}

pub(super) fn closed_frontend_handle() -> RuntimeFrontendHandle {
    let (sender, receiver) = mpsc::channel(1);
    drop(receiver);
    RuntimeFrontendHandle { sender }
}

pub(super) fn spawn_modality_worker(
    app: Arc<AppCore>,
    state: Arc<RuntimeState>,
) -> RuntimeModalityHandle {
    let (sender, receiver) = mpsc::channel(64);
    tokio::spawn(modality_worker(app, state, receiver));
    RuntimeModalityHandle { sender }
}

pub(super) fn spawn_llm_worker(app: Arc<AppCore>, state: Arc<RuntimeState>) -> RuntimeLlmHandle {
    let (sender, receiver) = mpsc::channel(64);
    tokio::spawn(llm_worker(app, state, receiver));
    RuntimeLlmHandle { sender }
}

pub(super) async fn shutdown_frontend(sender: &mpsc::Sender<FrontendCommand>) {
    let (response, receiver) = oneshot::channel();
    if sender
        .send(FrontendCommand::Shutdown(response))
        .await
        .is_ok()
    {
        let _ = receiver.await;
    }
}

pub(super) async fn shutdown_modalities(sender: &mpsc::Sender<ModalityCommand>) {
    let (response, receiver) = oneshot::channel();
    if sender
        .send(ModalityCommand::Shutdown(response))
        .await
        .is_ok()
    {
        let _ = receiver.await;
    }
}

pub(super) async fn shutdown_llms(sender: &mpsc::Sender<LlmCommand>) {
    let (response, receiver) = oneshot::channel();
    if sender.send(LlmCommand::Shutdown(response)).await.is_ok() {
        let _ = receiver.await;
    }
}

async fn frontend_worker(
    app: Arc<AppCore>,
    state: Arc<RuntimeState>,
    mut receiver: mpsc::Receiver<FrontendCommand>,
) {
    while let Some(command) = receiver.recv().await {
        if handle_frontend_command(app.clone(), &state, command) {
            return;
        }
    }
    stop_worker(&state.frontend);
}

fn handle_frontend_command(
    app: Arc<AppCore>,
    state: &RuntimeState,
    command: FrontendCommand,
) -> bool {
    match command {
        FrontendCommand::SpawnApp(work_area, response) => {
            let result = app.frontend().spawn_app(work_area);
            send_result(response, result);
        }
        FrontendCommand::UpdateCanvas(patch, response) => {
            let result = app.frontend().update_canvas(patch);
            send_result(response, result);
        }
        FrontendCommand::Canvas(response) => {
            let result = app.frontend().canvas();
            send_result(response, result);
        }
        FrontendCommand::Shutdown(response) => return shutdown_worker(&state.frontend, response),
    }
    false
}

async fn modality_worker(
    app: Arc<AppCore>,
    state: Arc<RuntimeState>,
    mut receiver: mpsc::Receiver<ModalityCommand>,
) {
    while let Some(command) = receiver.recv().await {
        if handle_modality_command(app.clone(), &state, command) {
            return;
        }
    }
    stop_worker(&state.modalities);
}

fn handle_modality_command(
    app: Arc<AppCore>,
    state: &RuntimeState,
    command: ModalityCommand,
) -> bool {
    match command {
        ModalityCommand::StartVoiceRecording {
            recording_id,
            media_type,
            response,
        } => send_start_voice_recording(app, recording_id, media_type, response),
        ModalityCommand::StreamVoiceAudio { chunk, response } => {
            let result = app.modalities().stream_voice_audio(chunk);
            send_result(response, result);
        }
        ModalityCommand::CurrentVoiceRecording(response) => {
            let result = app.modalities().current_voice_recording();
            send_result(response, result);
        }
        ModalityCommand::OpenVoicePlayback {
            playback_id,
            response,
        } => {
            let result = app.modalities().open_voice_playback(&playback_id);
            send_result(response, result);
        }
        ModalityCommand::AppendVoicePlaybackChunk { chunk, response } => {
            let result = app.modalities().append_voice_playback_chunk(chunk);
            send_result(response, result);
        }
        ModalityCommand::CloseVoicePlayback {
            playback_id,
            response,
        } => {
            let result = app.modalities().close_voice_playback(&playback_id);
            send_result(response, result);
        }
        ModalityCommand::SetVoicePlaybackStatus {
            playback_id,
            status,
            response,
        } => {
            let result = app
                .modalities()
                .set_voice_playback_status(&playback_id, status);
            send_result(response, result);
        }
        ModalityCommand::CurrentVoicePlaybackSegment(response) => {
            let result = app.modalities().current_voice_playback_segment();
            send_result(response, result);
        }
        ModalityCommand::RecordScreenshot {
            screenshot_id,
            media_type,
            bytes,
            response,
        } => send_modality_screenshot(app, screenshot_id, media_type, bytes, response),
        ModalityCommand::LatestScreenshot(response) => send_latest_screenshot(app, response),
        ModalityCommand::RecordDesktopBroadcast {
            broadcast_id,
            event_kind,
            payload,
            response,
        } => send_modality_broadcast(app, broadcast_id, event_kind, payload, response),
        ModalityCommand::LatestDesktopBroadcast(response) => send_latest_broadcast(app, response),
        ModalityCommand::Shutdown(response) => return shutdown_worker(&state.modalities, response),
    }
    false
}

async fn llm_worker(
    app: Arc<AppCore>,
    state: Arc<RuntimeState>,
    mut receiver: mpsc::Receiver<LlmCommand>,
) {
    while let Some(command) = receiver.recv().await {
        if handle_llm_command(app.clone(), &state, command) {
            return;
        }
    }
    stop_worker(&state.llms);
}

fn handle_llm_command(app: Arc<AppCore>, state: &RuntimeState, command: LlmCommand) -> bool {
    match command {
        LlmCommand::Complete {
            provider_id,
            request,
            response,
        } => {
            spawn_blocking_response(app, response, move |app| {
                app.llms().complete(&provider_id, &request)
            });
        }
        LlmCommand::ProviderIds(response) => {
            spawn_blocking_response(app, response, move |app| app.llms().provider_ids());
        }
        LlmCommand::Shutdown(response) => return shutdown_worker(&state.llms, response),
    }
    false
}

fn send_modality_screenshot(
    app: Arc<AppCore>,
    screenshot_id: String,
    media_type: String,
    bytes: Vec<u8>,
    response: Response<ScreenshotRecord>,
) {
    let result = app
        .modalities()
        .record_screenshot(&screenshot_id, &media_type, bytes);
    send_result(response, result);
}

fn send_start_voice_recording(
    app: Arc<AppCore>,
    recording_id: String,
    media_type: String,
    response: Response<super::FrontendRuntimeSnapshot>,
) {
    let result = app
        .modalities()
        .start_voice_recording(&recording_id, &media_type);
    send_result(response, result);
}

fn send_latest_screenshot(app: Arc<AppCore>, response: Response<Option<ScreenshotRecord>>) {
    let result = app.modalities().latest_screenshot();
    send_result(response, result);
}

fn send_modality_broadcast(
    app: Arc<AppCore>,
    broadcast_id: String,
    event_kind: String,
    payload: Value,
    response: Response<DesktopBroadcastRecord>,
) {
    let result = app
        .modalities()
        .record_desktop_broadcast(&broadcast_id, &event_kind, payload);
    send_result(response, result);
}

fn send_latest_broadcast(app: Arc<AppCore>, response: Response<Option<DesktopBroadcastRecord>>) {
    let result = app.modalities().latest_desktop_broadcast();
    send_result(response, result);
}

fn spawn_blocking_response<T, F>(app: Arc<AppCore>, response: Response<T>, task: F)
where
    T: Send + 'static,
    F: FnOnce(Arc<AppCore>) -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let result = task(app);
        send_result(response, result);
    });
}

fn send_result<T>(response: Response<T>, result: Result<T>) {
    let _ = response.send(result);
}

fn shutdown_worker(state: &AtomicU8, response: oneshot::Sender<()>) -> bool {
    stop_worker(state);
    let _ = response.send(());
    true
}
