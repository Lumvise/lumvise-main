//! Host-owned v1 adapters used by source-free compiled plugins.
//!
//! Plugin JSON stops here. Frontend and Neural Core types never cross the
//! subprocess protocol, and each adapter validates its own bounded contract.

use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

mod audio_sessions;
mod document_conversion;
mod speech_access;
use audio_sessions::{AUDIO_SESSION, LiveAudioConnections};
mod background_jobs;
use background_jobs::BackgroundJobAdapter;
mod canvas_images;
use canvas_images::add_image as frontend_canvas_add_image;
mod turn_wait;
use super::voice_playback_transport::{VoicePlaybackTransportEvent, sample_rate_from_media_type};
use turn_wait::TurnWaitAdapter;

use lumvise_db_core::SemanticPersistence;
use lumvise_frontend_core::{
    AssistantEngine, CanvasPatch, DashboardView, FrontendCore, MAIN_CANVAS_ID, OrbMode,
    VoicePlaybackChunk, VoicePlaybackStatus,
};
use lumvise_neural_core::llm_providers::{
    LlmExecutionControl, LlmExecutorRegistry, LlmFailure, LlmMcpServerConfig, LlmMessage,
    LlmRequest, LlmRequestOptions, LlmResponseFormat,
};
use lumvise_neural_core::text2voice::{Text2VoiceRequest, Text2VoiceStreamEvent};
use lumvise_neural_core::{
    LlmProviderRegistry, SpeechExecutionError, SpeechRecognizer, SpeechSynthesizer,
    SpeechToTextExecutor, TextToSpeechExecutor,
};
use lumvise_plugin_runtime::{
    ExclusiveInvocationLanes, HostCapabilityError, PluginInvocationContext,
};
use lumvise_resource_routing::InvocationControl;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::host_capability_catalog::{
    ASSISTANT_ENGINE_AVAILABILITY, BACKGROUND_JOB, EXCLUSIVE_LANE, FRONTEND_ACTION,
    FRONTEND_CANVAS, NEURAL_LLM, PROJECT_EXECUTION, SCOPED_MCP, SPEECH_AVAILABILITY,
    SPEECH_TO_TEXT, TEXT_TO_SPEECH, TURN_WAIT,
};

const MAX_LLM_MESSAGES: usize = 128;
const MAX_LLM_SERVERS: usize = 16;
const MAX_LLM_TEXT_BYTES: usize = 256 * 1024;
const MAX_TTS_TEXT_BYTES: usize = 64 * 1024;
const MAX_CANVAS_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_CANVAS_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_LLM_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

type SharedSpeechRecognizer = Arc<Mutex<Option<Arc<dyn SpeechRecognizer>>>>;
type SharedSpeechSynthesizer = Arc<Mutex<Option<Arc<dyn SpeechSynthesizer>>>>;

pub(super) struct InvocationControlBridge {
    control: InvocationControl,
    monitoring: Arc<AtomicBool>,
}

impl InvocationControlBridge {
    pub(super) fn control(&self) -> InvocationControl {
        self.control.clone()
    }
}

impl Drop for InvocationControlBridge {
    fn drop(&mut self) {
        self.monitoring.store(false, Ordering::Release);
    }
}

/// Shared host state constructed before production plugin restore.
pub(crate) struct PluginHostServices {
    semantic: Arc<dyn SemanticPersistence>,
    audio_connections: LiveAudioConnections,
    frontend: Arc<Mutex<FrontendCore>>,
    llms: Arc<Mutex<LlmProviderRegistry>>,
    llm_executors: Arc<LlmExecutorRegistry>,
    assistant_llm_executors: Arc<LlmExecutorRegistry>,
    activity: Arc<crate::workspace_activity::WorkspaceActivity>,
    frontend_actions: Arc<Mutex<Vec<Value>>>,
    frontend_action_ids: Mutex<HashSet<String>>,
    scoped_mcp_base_url: Arc<Mutex<Option<String>>>,
    exclusive_lanes: Arc<ExclusiveInvocationLanes>,
    speech_recognizer: SharedSpeechRecognizer,
    speech_synthesizer: SharedSpeechSynthesizer,
    speech_to_text_executor: Mutex<Option<(Arc<dyn SpeechRecognizer>, Arc<SpeechToTextExecutor>)>>,
    text_to_speech_executor: Mutex<Option<(Arc<dyn SpeechSynthesizer>, Arc<TextToSpeechExecutor>)>>,
    voice_playback_transport: Arc<super::voice_playback_transport::VoicePlaybackTransport>,
    playback_owners: Arc<Mutex<HashMap<String, PlaybackOwner>>>,
    playback_controls: Arc<Mutex<HashMap<String, InvocationControl>>>,
    playback_opened: Arc<Mutex<HashSet<String>>>,
    playback_sequences: Arc<Mutex<HashMap<String, u64>>>,
    background_jobs: BackgroundJobAdapter,
    turn_wait: TurnWaitAdapter,
    project_execution: Mutex<Option<crate::ProjectExecutionService>>,
}

/// Correlation recorded when a `text_to_speech` segment opens, so a later
/// real playback-status report (`report_voice_playback_status`) can notify
/// whichever plugin/session opened it — kept as opaque strings on purpose,
/// this module never interprets what `session_id` means.
struct PlaybackOwner {
    plugin_id: String,
    session_id: String,
}

impl PluginHostServices {
    pub(crate) fn new(
        frontend: FrontendCore,
        llms: LlmProviderRegistry,
        semantic: Arc<dyn SemanticPersistence>,
    ) -> Arc<Self> {
        let llms = Arc::new(Mutex::new(llms));
        let llm_executors = LlmExecutorRegistry::new(Arc::clone(&llms));
        let assistant_llm_executors = LlmExecutorRegistry::new(Arc::clone(&llms));
        let activity = Arc::new(crate::workspace_activity::WorkspaceActivity::default());
        let frontend = Arc::new(Mutex::new(frontend));
        let speech_recognizer = Arc::new(Mutex::new(None));
        let speech_synthesizer = Arc::new(Mutex::new(None));
        Arc::new(Self {
            semantic,
            audio_connections: LiveAudioConnections::default(),
            frontend: Arc::clone(&frontend),
            llms: Arc::clone(&llms),
            llm_executors: Arc::clone(&llm_executors),
            assistant_llm_executors: Arc::clone(&assistant_llm_executors),
            activity: Arc::clone(&activity),
            frontend_actions: Arc::new(Mutex::new(Vec::new())),
            frontend_action_ids: Mutex::new(HashSet::new()),
            scoped_mcp_base_url: Arc::new(Mutex::new(None)),
            exclusive_lanes: Arc::new(ExclusiveInvocationLanes::new()),
            speech_recognizer,
            speech_synthesizer,
            speech_to_text_executor: Mutex::new(None),
            text_to_speech_executor: Mutex::new(None),
            voice_playback_transport: Arc::new(
                super::voice_playback_transport::VoicePlaybackTransport::new(),
            ),
            playback_owners: Arc::new(Mutex::new(HashMap::new())),
            playback_controls: Arc::new(Mutex::new(HashMap::new())),
            playback_opened: Arc::new(Mutex::new(HashSet::new())),
            playback_sequences: Arc::new(Mutex::new(HashMap::new())),
            background_jobs: BackgroundJobAdapter::new(
                llm_executors,
                assistant_llm_executors,
                frontend,
                activity,
            ),
            turn_wait: TurnWaitAdapter::new(),
            project_execution: Mutex::new(None),
        })
    }

    /// Shared handle to the bounded voice-playback event transport; the
    /// desktop bridge's `subscribe_voice_playback` command is the one
    /// consumer, `text_to_speech` below the one producer.
    pub(crate) fn voice_playback_transport(
        &self,
    ) -> Arc<super::voice_playback_transport::VoicePlaybackTransport> {
        Arc::clone(&self.voice_playback_transport)
    }

    pub(crate) fn frontend(&self) -> Arc<Mutex<FrontendCore>> {
        Arc::clone(&self.frontend)
    }

    pub(crate) fn llms(&self) -> Arc<Mutex<LlmProviderRegistry>> {
        Arc::clone(&self.llms)
    }

    pub(crate) fn activity(&self) -> Arc<crate::workspace_activity::WorkspaceActivity> {
        Arc::clone(&self.activity)
    }

    /// Shared per-provider LLM executor pool (T6.4). Routing desktop/UI
    /// completions through this gives HTTP providers bounded parallelism and
    /// stops a slow provider from serializing against unrelated providers.
    pub(crate) fn llm_executors(&self) -> Arc<LlmExecutorRegistry> {
        Arc::clone(&self.llm_executors)
    }

    pub(crate) fn frontend_actions(&self) -> Arc<Mutex<Vec<Value>>> {
        Arc::clone(&self.frontend_actions)
    }

    pub(crate) fn scoped_mcp_base_url(&self) -> Arc<Mutex<Option<String>>> {
        Arc::clone(&self.scoped_mcp_base_url)
    }

    pub(crate) fn exclusive_lanes(&self) -> Arc<ExclusiveInvocationLanes> {
        Arc::clone(&self.exclusive_lanes)
    }

    pub(crate) fn install_speech_recognizer(&self, service: Arc<dyn SpeechRecognizer>) {
        if let Ok(mut slot) = self.speech_recognizer.lock() {
            *slot = Some(service);
        }
    }

    pub(crate) fn install_speech_synthesizer(&self, service: Arc<dyn SpeechSynthesizer>) {
        if let Ok(mut slot) = self.speech_synthesizer.lock() {
            *slot = Some(service);
        }
    }

    pub(crate) fn install_project_execution(&self, service: crate::ProjectExecutionService) {
        if let Ok(mut slot) = self.project_execution.lock() {
            *slot = Some(service);
        }
    }

    pub(crate) fn invoke(
        &self,
        plugin_id: &str,
        capability_id: &str,
        input: Value,
    ) -> Result<Value, HostCapabilityError> {
        match capability_id {
            FRONTEND_ACTION => self.frontend_action(input),
            FRONTEND_CANVAS => self.frontend_canvas(plugin_id, input, self.semantic.as_ref()),
            NEURAL_LLM => self.neural_llm(
                plugin_id,
                input,
                LlmExecutionControl::new(InvocationControl::sixty_seconds()),
            ),
            SPEECH_TO_TEXT => self.speech_to_text(input, InvocationControl::sixty_seconds()),
            TEXT_TO_SPEECH => self.text_to_speech(plugin_id, input),
            BACKGROUND_JOB => self.background_jobs.invoke(plugin_id, input),
            TURN_WAIT => self.turn_wait.invoke(plugin_id, input),
            PROJECT_EXECUTION => self.project_execution(plugin_id, input),
            SCOPED_MCP => self.scoped_mcp(input),
            EXCLUSIVE_LANE => self.exclusive_lane(plugin_id, input),
            ASSISTANT_ENGINE_AVAILABILITY => self.assistant_engine_availability(),
            SPEECH_AVAILABILITY => self.speech_availability(),
            super::host_capability_catalog::DOCUMENT_CONVERSION_OPTIONS => {
                self.document_conversion_options()
            }
            AUDIO_SESSION => self.audio_session(input),
            other => Err(unknown(other)),
        }
    }

    pub(crate) fn invoke_controlled(
        &self,
        plugin_id: &str,
        capability_id: &str,
        input: Value,
        context: &PluginInvocationContext,
    ) -> Result<Value, HostCapabilityError> {
        let control = invocation_control(context);
        match capability_id {
            NEURAL_LLM => self.neural_llm(
                plugin_id,
                input,
                LlmExecutionControl::new(control.control()),
            ),
            SPEECH_TO_TEXT => self.speech_to_text(input, control.control()),
            TEXT_TO_SPEECH => self.text_to_speech(plugin_id, input),
            _ => self.invoke(plugin_id, capability_id, input),
        }
    }

    /// Whether the app has an Assistant Engine selected and active — reuses
    /// the exact background-job provider/model resolution so the two never
    /// drift (issue #49).
    fn assistant_engine_availability(&self) -> Result<Value, HostCapabilityError> {
        let (provider, model) =
            configured_llm_selection(&self.frontend, ASSISTANT_ENGINE_AVAILABILITY)?;
        Ok(json!({"available": provider.is_some() && model.is_some()}))
    }

    fn project_execution(
        &self,
        plugin_id: &str,
        input: Value,
    ) -> Result<Value, HostCapabilityError> {
        let request: ProjectExecutionHostRequest = decode(PROJECT_EXECUTION, input)?;
        let service = self
            .project_execution
            .lock()
            .map_err(|_| failed(PROJECT_EXECUTION, "project execution slot mutex poisoned"))?
            .clone()
            .ok_or_else(|| unavailable(PROJECT_EXECUTION, "project execution is not installed"))?;
        let job = match request {
            ProjectExecutionHostRequest::Submit {
                project_root,
                capability_id,
                idempotency_key,
                input,
                priority,
            } => service.submit(crate::ProjectExecutionRequest {
                requester_id: plugin_id.to_string(),
                project_root,
                capability_id,
                idempotency_key,
                input,
                priority: priority.unwrap_or_default(),
            }),
            ProjectExecutionHostRequest::Status { job_id } => service.status(plugin_id, &job_id),
            ProjectExecutionHostRequest::Cancel { job_id } => service.cancel(plugin_id, &job_id),
        }
        .map_err(project_execution_error)?;
        serde_json::to_value(job).map_err(|error| failed(PROJECT_EXECUTION, &error.to_string()))
    }

    fn frontend_action(&self, input: Value) -> Result<Value, HostCapabilityError> {
        let request: FrontendActionRequest = decode(FRONTEND_ACTION, input)?;
        require_non_empty(FRONTEND_ACTION, &request.action, "action")?;
        let mut action_ids = if request.action_id.is_some() {
            Some(
                self.frontend_action_ids
                    .lock()
                    .map_err(|_| failed(FRONTEND_ACTION, "frontend action id mutex poisoned"))?,
            )
        } else {
            None
        };
        if let Some(action_id) = request.action_id.as_deref() {
            require_non_empty(FRONTEND_ACTION, action_id, "action_id")?;
            if action_ids
                .as_ref()
                .is_some_and(|seen| seen.contains(action_id))
            {
                return Ok(json!({"accepted": true, "duplicate": true}));
            }
        }
        let mut queued = json!({"action": request.action, "payload": request.payload});
        if let Some(action_id) = request.action_id.as_ref() {
            queued["action_id"] = Value::String(action_id.clone());
        }
        let action = queued["action"].as_str().unwrap_or_default();
        let response = if action.starts_with("frontend.") {
            self.apply_frontend_action(action, queued["payload"].clone())?
        } else {
            json!({"accepted": true})
        };
        if let (Some(action_id), Some(seen)) = (request.action_id.as_ref(), action_ids.as_mut()) {
            seen.insert(action_id.clone());
        }
        self.frontend_actions
            .lock()
            .map_err(|_| failed(FRONTEND_ACTION, "frontend action queue mutex poisoned"))?
            .push(queued);
        Ok(response)
    }

    fn apply_frontend_action(
        &self,
        action: &str,
        payload: Value,
    ) -> Result<Value, HostCapabilityError> {
        let mut frontend = self
            .frontend
            .lock()
            .map_err(|_| failed(FRONTEND_ACTION, "frontend mutex poisoned"))?;
        let snapshot = match action {
            "frontend.open_dashboard" => match payload.get("view").and_then(Value::as_str) {
                Some("canvas_dashboard") => {
                    frontend.open_dashboard_view(DashboardView::CanvasDashboard)
                }
                _ => {
                    return Err(invalid(
                        FRONTEND_ACTION,
                        &payload,
                        "supported dashboard view",
                    ));
                }
            },
            "frontend.prepare_assistant_session" | "frontend.deliver_assistant_response" => {
                return Ok(json!({"accepted": true, "deferred": true}));
            }
            "frontend.start_countdown" => frontend.start_countdown(optional_digit(&payload)?),
            "frontend.set_orb_mode" => frontend.set_orb_mode(parse_orb_mode(&payload)?),
            other => {
                return Err(invalid(
                    FRONTEND_ACTION,
                    &Value::String(other.into()),
                    "supported generic frontend action",
                ));
            }
        }
        .map_err(|error| failed(FRONTEND_ACTION, &error.to_string()))?;
        serde_json::to_value(snapshot).map_err(|error| failed(FRONTEND_ACTION, &error.to_string()))
    }

    fn frontend_canvas(
        &self,
        plugin_id: &str,
        input: Value,
        semantic: &dyn SemanticPersistence,
    ) -> Result<Value, HostCapabilityError> {
        bounded_input(FRONTEND_CANVAS, &input, MAX_CANVAS_INPUT_BYTES)?;
        let fields = exact_object(FRONTEND_CANVAS, &input)?;
        let operation = required_string(FRONTEND_CANVAS, fields, "operation")?;
        // Image sources may block (a bounded URL fetch), so `add_image` runs
        // without the frontend lock and takes it only for the diff.
        if operation == "add_image" {
            let value = frontend_canvas_add_image(plugin_id, fields, &self.frontend)?;
            return bounded_output(FRONTEND_CANVAS, value, MAX_CANVAS_OUTPUT_BYTES);
        }
        let mut frontend = self
            .frontend
            .lock()
            .map_err(|_| failed(FRONTEND_CANVAS, "frontend mutex poisoned"))?;
        let value = match operation {
            "get" => {
                exact_fields(FRONTEND_CANVAS, fields, &["operation", "canvas_id"])?;
                serde_json::to_value(frontend.canvas(optional_canvas_id(FRONTEND_CANVAS, fields)?))
                    .map_err(|error| failed(FRONTEND_CANVAS, &error.to_string()))
            }
            "update" => {
                exact_fields(
                    FRONTEND_CANVAS,
                    fields,
                    &["operation", "canvas_id", "patch"],
                )?;
                let patch: CanvasPatch =
                    decode(FRONTEND_CANVAS, required(fields, "patch")?.clone())?;
                serde_json::to_value(
                    frontend
                        .update_canvas(optional_canvas_id(FRONTEND_CANVAS, fields)?, patch)
                        .map_err(|error| failed(FRONTEND_CANVAS, &error.to_string()))?,
                )
                .map_err(|error| failed(FRONTEND_CANVAS, &error.to_string()))
            }
            "apply_diff" => {
                exact_fields(
                    FRONTEND_CANVAS,
                    fields,
                    &["operation", "canvas_id", "patch", "base_revision"],
                )?;
                let source = format!("plugin:{plugin_id}");
                serde_json::to_value(
                    frontend
                        .apply_canvas_diff(
                            required_string(FRONTEND_CANVAS, fields, "canvas_id")?,
                            required(fields, "patch")?.clone(),
                            &source,
                            fields
                                .get("base_revision")
                                .filter(|value| !value.is_null())
                                .map(|value| {
                                    value.as_u64().ok_or_else(|| {
                                        invalid(
                                            FRONTEND_CANVAS,
                                            value,
                                            "non-negative integer base_revision",
                                        )
                                    })
                                })
                                .transpose()?,
                        )
                        .map_err(|error| failed(FRONTEND_CANVAS, &error.to_string()))?,
                )
                .map_err(|error| failed(FRONTEND_CANVAS, &error.to_string()))
            }
            "user_changes" => {
                exact_fields(
                    FRONTEND_CANVAS,
                    fields,
                    &["operation", "canvas_id", "since_revision"],
                )?;
                let since = required(fields, "since_revision")?
                    .as_u64()
                    .ok_or_else(|| {
                        invalid(
                            FRONTEND_CANVAS,
                            required(fields, "since_revision").unwrap_or(&Value::Null),
                            "non-negative integer since_revision",
                        )
                    })?;
                Ok(frontend
                    .user_canvas_changes(
                        required_string(FRONTEND_CANVAS, fields, "canvas_id")?,
                        since,
                    )
                    .map_err(|error| failed(FRONTEND_CANVAS, &error.to_string()))?)
            }
            "export_files" => {
                exact_fields(
                    FRONTEND_CANVAS,
                    fields,
                    &["operation", "artifact_id", "canvas_id"],
                )?;
                crate::app::export_canvas_files(
                    semantic,
                    required_string(FRONTEND_CANVAS, fields, "artifact_id")?,
                    &frontend
                        .canvas(optional_canvas_id(FRONTEND_CANVAS, fields)?)
                        .document,
                )
                .map_err(|error| failed(FRONTEND_CANVAS, &error.to_string()))
            }
            "discard" => {
                exact_fields(FRONTEND_CANVAS, fields, &["operation", "canvas_id"])?;
                let discarded = frontend
                    .discard_canvas(required_string(FRONTEND_CANVAS, fields, "canvas_id")?)
                    .map_err(|error| failed(FRONTEND_CANVAS, &error.to_string()))?;
                Ok(json!({ "discarded": discarded }))
            }
            other => {
                return Err(invalid(
                    FRONTEND_CANVAS,
                    &json!(other),
                    "operation get, update, apply_diff, export_files, add_image, user_changes, or discard",
                ));
            }
        };
        let value = value.map_err(|error| failed(FRONTEND_CANVAS, &error.to_string()))?;
        bounded_output(FRONTEND_CANVAS, value, MAX_CANVAS_OUTPUT_BYTES)
    }

    fn neural_llm(
        &self,
        plugin_id: &str,
        input: Value,
        control: LlmExecutionControl,
    ) -> Result<Value, HostCapabilityError> {
        let mut request: NeutralLlmRequest = decode(NEURAL_LLM, input)?;
        request.validate()?;
        let provider_id = resolve_llm_selection(&self.frontend, NEURAL_LLM, &mut request)?;
        let ticket = self.activity.track_llm(plugin_id, &provider_id);
        let result = selected_llm_executor(
            plugin_id,
            &self.llm_executors,
            &self.assistant_llm_executors,
        )
        .complete(&provider_id, request.into_neural(), ticket.control(control));
        ticket.finish(&result);
        let response = result.map_err(llm_execution_error)?;
        bounded_output(
            NEURAL_LLM,
            json!({"response": response}),
            MAX_LLM_OUTPUT_BYTES,
        )
    }

    fn speech_to_text(
        &self,
        input: Value,
        control: InvocationControl,
    ) -> Result<Value, HostCapabilityError> {
        let request: SpeechToTextRequest = decode(SPEECH_TO_TEXT, input)?;
        if request.operation != "transcribe_current_recording" {
            return Err(invalid(
                SPEECH_TO_TEXT,
                &json!(request.operation),
                "operation transcribe_current_recording",
            ));
        }
        let recording = self
            .frontend
            .lock()
            .map_err(|_| failed(SPEECH_TO_TEXT, "frontend mutex poisoned"))?
            .current_voice_recording()
            .ok_or_else(|| {
                unavailable(SPEECH_TO_TEXT, "no completed voice recording is available")
            })?;
        let response = self
            .speech_to_text_executor()?
            .transcribe(
                lumvise_neural_core::voice2text::Voice2TextRequest {
                    audio: recording.audio,
                    media_type: recording.media_type,
                    model: request.model,
                },
                control,
            )
            .map_err(|error| speech_execution_error(SPEECH_TO_TEXT, error))?;
        Ok(json!({"transcript": response.transcript}))
    }

    fn text_to_speech(&self, plugin_id: &str, input: Value) -> Result<Value, HostCapabilityError> {
        let request: TextToSpeechRequest = decode(TEXT_TO_SPEECH, input)?;
        require_non_empty(TEXT_TO_SPEECH, &request.playback_id, "playback_id")?;
        require_non_empty(TEXT_TO_SPEECH, &request.session_id, "session_id")?;

        if request.cancel {
            return self.cancel_text_to_speech(&request.playback_id);
        }

        let text = request.text.as_deref().unwrap_or_default();
        if text.is_empty() && !request.close {
            require_non_empty(TEXT_TO_SPEECH, text, "text")?;
        }
        if text.len() > MAX_TTS_TEXT_BYTES {
            return Err(quota(TEXT_TO_SPEECH, text.len(), MAX_TTS_TEXT_BYTES));
        }

        let executor = self.text_to_speech_executor()?;
        let segment_index = if text.is_empty() {
            self.current_playback_segment_index(&request.playback_id)?
        } else {
            self.open_or_append_playback_segment(&request)?
        };
        self.remember_playback_owner(&request.playback_id, plugin_id, &request.session_id);

        let synth_control = self.playback_control(&request.playback_id);

        let transport = self.voice_playback_transport();
        let playback_id = request.playback_id.clone();
        let close_after = request.close;
        let frontend = Arc::clone(&self.frontend);
        let opened_map = Arc::clone(&self.playback_opened);
        let sequences_map = Arc::clone(&self.playback_sequences);
        let controls_map = Arc::clone(&self.playback_controls);

        let enqueue_result = executor.enqueue_stream(
            Text2VoiceRequest {
                text: text.to_string(),
                voice_id: request.voice_id.clone(),
                model: request.model.clone(),
            },
            synth_control,
            move |event| {
                match event {
                    Text2VoiceStreamEvent::AudioChunk {
                        sequence,
                        audio,
                        media_type,
                    } => {
                        let first_chunk = opened_map
                            .lock()
                            .map(|mut opened| opened.insert(playback_id.clone()))
                            .unwrap_or(false);
                        if first_chunk {
                            let _ = transport.publish(VoicePlaybackTransportEvent::Opened {
                                playback_id: playback_id.clone(),
                                media_type: media_type.clone(),
                                sample_rate_hz: sample_rate_from_media_type(&media_type),
                            });
                        }
                        let transport_sequence = sequences_map
                            .lock()
                            .map(|mut sequences| {
                                let next = sequences.entry(playback_id.clone()).or_insert(0);
                                let sequence = *next;
                                *next = next.saturating_add(1);
                                sequence
                            })
                            .unwrap_or(sequence);
                        let _ = transport.publish(VoicePlaybackTransportEvent::AudioChunk {
                            playback_id: playback_id.clone(),
                            sequence: transport_sequence,
                            audio,
                        });
                    }
                    Text2VoiceStreamEvent::Complete => {
                        if close_after {
                            let _ = transport.publish(VoicePlaybackTransportEvent::Closed {
                                playback_id: playback_id.clone(),
                            });
                            if let Ok(mut frontend) = frontend.lock() {
                                let _ = frontend.close_voice_playback(playback_id.clone());
                            }
                            cleanup_synthesis(
                                &controls_map,
                                &opened_map,
                                &sequences_map,
                                &playback_id,
                            );
                        }
                    }
                    Text2VoiceStreamEvent::Error { .. } => {
                        let _ = transport.publish(VoicePlaybackTransportEvent::Cancelled {
                            playback_id: playback_id.clone(),
                        });
                        if let Ok(mut frontend) = frontend.lock() {
                            let _ = frontend.set_voice_playback_status(
                                playback_id.clone(),
                                VoicePlaybackStatus::Failed,
                            );
                        }
                        cleanup_synthesis(&controls_map, &opened_map, &sequences_map, &playback_id);
                    }
                }
                Ok(())
            },
        );
        enqueue_result.map_err(|error| speech_execution_error(TEXT_TO_SPEECH, error))?;

        Ok(json!({
            "playback_id": request.playback_id,
            "segment_index": segment_index,
            "accepted": true,
        }))
    }

    /// Opens a new playback turn on first use for `playback_id`, or appends
    /// the next segment when the turn is already active, returning the
    /// assigned `segment_index`.
    fn open_or_append_playback_segment(
        &self,
        request: &TextToSpeechRequest,
    ) -> Result<u64, HostCapabilityError> {
        let mut frontend = self
            .frontend
            .lock()
            .map_err(|_| failed(TEXT_TO_SPEECH, "frontend mutex poisoned"))?;
        let is_new_turn = frontend
            .voice_playback_status()
            .active_playback_id
            .as_deref()
            != Some(request.playback_id.as_str());
        if is_new_turn {
            frontend
                .open_voice_playback(request.playback_id.clone())
                .map_err(|error| failed(TEXT_TO_SPEECH, &error.to_string()))?;
        }
        let snapshot = frontend
            .append_voice_playback_chunk(VoicePlaybackChunk {
                playback_id: request.playback_id.clone(),
                media_type: "audio/pcm".to_string(),
            })
            .map_err(|error| failed(TEXT_TO_SPEECH, &error.to_string()))?;
        Ok(snapshot
            .state
            .voice_playback
            .segments
            .back()
            .map(|segment| segment.segment_index)
            .unwrap_or(0))
    }

    fn current_playback_segment_index(
        &self,
        playback_id: &str,
    ) -> Result<u64, HostCapabilityError> {
        let frontend = self
            .frontend
            .lock()
            .map_err(|_| failed(TEXT_TO_SPEECH, "frontend mutex poisoned"))?;
        frontend
            .voice_playback_status()
            .segments
            .iter()
            .rev()
            .find(|segment| segment.playback_id == playback_id)
            .map(|segment| segment.segment_index)
            .ok_or_else(|| unavailable(TEXT_TO_SPEECH, "no playback segment is open to close"))
    }

    fn remember_playback_owner(&self, playback_id: &str, plugin_id: &str, session_id: &str) {
        if let Ok(mut map) = self.playback_owners.lock() {
            map.insert(
                playback_id.to_string(),
                PlaybackOwner {
                    plugin_id: plugin_id.to_string(),
                    session_id: session_id.to_string(),
                },
            );
        }
    }

    fn playback_control(&self, playback_id: &str) -> InvocationControl {
        self.playback_controls
            .lock()
            .map(|mut controls| {
                controls
                    .entry(playback_id.to_string())
                    .or_insert_with(|| InvocationControl::with_deadline(Duration::from_secs(600)))
                    .clone()
            })
            .unwrap_or_else(|_| InvocationControl::with_deadline(Duration::from_secs(600)))
    }

    /// Cancels an outstanding stream for `playback_id` (barge-in): stops the
    /// background synthesis worker, evicts owner/control tracking, and
    /// marks every queued segment `Cancelled`.
    fn cancel_text_to_speech(&self, playback_id: &str) -> Result<Value, HostCapabilityError> {
        if let Ok(map) = self.playback_controls.lock()
            && let Some(control) = map.get(playback_id)
        {
            control.cancel();
        }
        if let Ok(mut map) = self.playback_owners.lock() {
            map.remove(playback_id);
        }
        cleanup_synthesis(
            &self.playback_controls,
            &self.playback_opened,
            &self.playback_sequences,
            playback_id,
        );
        let _ = self
            .voice_playback_transport
            .publish(VoicePlaybackTransportEvent::Cancelled {
                playback_id: playback_id.to_string(),
            });
        let mut frontend = self
            .frontend
            .lock()
            .map_err(|_| failed(TEXT_TO_SPEECH, "frontend mutex poisoned"))?;
        frontend
            .cancel_voice_playback(playback_id)
            .map_err(|error| failed(TEXT_TO_SPEECH, &error.to_string()))?;
        Ok(json!({"playback_id": playback_id, "accepted": true, "cancelled": true}))
    }

    /// Applies a real renderer-reported playback status transition and
    pub(crate) fn report_voice_playback_status(
        &self,
        playback_id: &str,
        status: VoicePlaybackStatus,
    ) -> Result<Option<(String, String, u64)>, HostCapabilityError> {
        if self.report_audio_playback(playback_id, status) {
            return Ok(None);
        }
        let owner = self.playback_owners.lock().ok().and_then(|map| {
            map.get(playback_id)
                .map(|owner| (owner.plugin_id.clone(), owner.session_id.clone()))
        });
        let mut frontend = self
            .frontend
            .lock()
            .map_err(|_| failed(TEXT_TO_SPEECH, "frontend mutex poisoned"))?;
        let state = frontend.voice_playback_status();
        if state.active_playback_id.as_deref() != Some(playback_id) {
            // The renderer reported a playback turn that is no longer the
            // active one (the turn was replaced by the next response, or the
            // session was replaced). That is normal lifecycle noise: the
            // active turn's own reports drive the session. Failing here used
            // to surface as a bogus session last_error ("expected active
            // voice playback id") and degrade live voice sessions.
            drop(frontend);
            if let Ok(mut map) = self.playback_owners.lock() {
                map.remove(playback_id);
            }
            cleanup_synthesis(
                &self.playback_controls,
                &self.playback_opened,
                &self.playback_sequences,
                playback_id,
            );
            return Ok(None);
        }
        let segment_index = state
            .segments
            .iter()
            .rev()
            .find(|segment| segment.playback_id == playback_id)
            .map(|segment| segment.segment_index)
            .unwrap_or(0);
        frontend
            .set_voice_playback_status(playback_id, status)
            .map_err(|error| failed(TEXT_TO_SPEECH, &error.to_string()))?;
        drop(frontend);
        if matches!(
            status,
            VoicePlaybackStatus::Completed
                | VoicePlaybackStatus::Cancelled
                | VoicePlaybackStatus::Failed
        ) {
            if let Ok(mut map) = self.playback_owners.lock() {
                map.remove(playback_id);
            }
            cleanup_synthesis(
                &self.playback_controls,
                &self.playback_opened,
                &self.playback_sequences,
                playback_id,
            );
        }
        Ok(owner.map(|(plugin_id, session_id)| (plugin_id, session_id, segment_index)))
    }

    fn scoped_mcp(&self, input: Value) -> Result<Value, HostCapabilityError> {
        let request: OperationRequest = decode(SCOPED_MCP, input)?;
        if request.operation != "base_url" {
            return Err(invalid(
                SCOPED_MCP,
                &json!(request.operation),
                "operation base_url",
            ));
        }
        let value = self
            .scoped_mcp_base_url
            .lock()
            .map_err(|_| failed(SCOPED_MCP, "Scoped MCP URL mutex poisoned"))?
            .clone();
        Ok(json!({"base_url": value}))
    }

    fn exclusive_lane(&self, plugin_id: &str, input: Value) -> Result<Value, HostCapabilityError> {
        let request: LaneRequest = decode(EXCLUSIVE_LANE, input)?;
        if request.operation != "snapshot" {
            return Err(invalid(
                EXCLUSIVE_LANE,
                &json!(request.operation),
                "operation snapshot",
            ));
        }
        require_non_empty(EXCLUSIVE_LANE, &request.lane_id, "lane_id")?;
        require_non_empty(EXCLUSIVE_LANE, &request.owner_id, "owner_id")?;
        let snapshot = self
            .exclusive_lanes
            .snapshot(
                plugin_id,
                &request.lane_id,
                &request.owner_id,
                request.session_id.as_deref(),
            )
            .map_err(|error| failed(EXCLUSIVE_LANE, &error.to_string()))?;
        Ok(json!({"active_session_id": snapshot.active_session_id,
            "active_elapsed_ms": snapshot.active_elapsed_ms,
            "queued": snapshot.queued, "caller": {"status": snapshot.caller_status,
            "position": snapshot.caller_position}}))
    }
}

fn selected_llm_executor<'a>(
    plugin_id: &str,
    ordinary: &'a Arc<LlmExecutorRegistry>,
    assistant: &'a Arc<LlmExecutorRegistry>,
) -> &'a Arc<LlmExecutorRegistry> {
    if plugin_id == "builtin.assistant" {
        assistant
    } else {
        ordinary
    }
}

/// Converts the Plugin Runtime lifecycle into the Neural Core lifecycle at the
/// App Core boundary. Explicit runtime cancellation remains cancellation; the
/// runtime deadline becomes the shared control deadline so the executor can
/// classify it as a recoverable provider-turn timeout.
pub(super) fn invocation_control(context: &PluginInvocationContext) -> InvocationControlBridge {
    let deadline = context.deadline();
    let control = InvocationControl::with_deadline(
        deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(60)),
    );
    let cancellation = context.cancellation();
    let monitoring = Arc::new(AtomicBool::new(true));
    let monitor = Arc::clone(&monitoring);
    let forwarded = control.clone();
    std::thread::spawn(move || {
        loop {
            if !monitor.load(Ordering::Acquire) || forwarded.is_expired() {
                return;
            }
            if cancellation.is_cancelled() {
                forwarded.cancel();
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    InvocationControlBridge {
        control,
        monitoring,
    }
}

fn llm_execution_error(error: LlmFailure) -> HostCapabilityError {
    failed(NEURAL_LLM, &error.message)
}

fn speech_execution_error(capability: &str, error: SpeechExecutionError) -> HostCapabilityError {
    match error {
        SpeechExecutionError::Service(message) => failed(capability, &message),
        other => unavailable(capability, &other.to_string()),
    }
}

fn cleanup_synthesis(
    controls: &Mutex<HashMap<String, InvocationControl>>,
    opened: &Mutex<HashSet<String>>,
    sequences: &Mutex<HashMap<String, u64>>,
    playback_id: &str,
) {
    if let Ok(mut map) = controls.lock() {
        map.remove(playback_id);
    }
    if let Ok(mut set) = opened.lock() {
        set.remove(playback_id);
    }
    if let Ok(mut map) = sequences.lock() {
        map.remove(playback_id);
    }
}

fn project_execution_error(error: crate::ProjectExecutionError) -> HostCapabilityError {
    match error {
        crate::ProjectExecutionError::CapacityExceeded { actual, maximum } => {
            quota(PROJECT_EXECUTION, actual, maximum)
        }
        crate::ProjectExecutionError::ProviderQueueFull { .. } => HostCapabilityError::new(
            PROJECT_EXECUTION,
            "host_capability_quota_exceeded",
            format!("{PROJECT_EXECUTION} quota exceeded: {error}"),
            true,
        ),
        crate::ProjectExecutionError::ProviderUnavailable { .. }
        | crate::ProjectExecutionError::JobNotFound { .. } => {
            unavailable(PROJECT_EXECUTION, &error.to_string())
        }
        error => failed(PROJECT_EXECUTION, &error.to_string()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FrontendActionRequest {
    #[serde(default)]
    action_id: Option<String>,
    action: String,
    payload: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpeechToTextRequest {
    operation: String,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextToSpeechRequest {
    playback_id: String,
    /// Opaque routing correlation supplied by the calling plugin (its own
    /// domain session id) — never interpreted here, only stored so a later
    /// playback-status report can notify the right owner (W2).
    session_id: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    voice_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    /// Marks this call's segment as the turn's last: the stream closes
    /// (`Closed` transport event + `close_voice_playback`) once synthesis
    /// of this segment completes.
    #[serde(default)]
    close: bool,
    /// Cancels the outstanding stream for `playback_id` instead of
    /// synthesizing (barge-in); every other field is ignored.
    #[serde(default)]
    cancel: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationRequest {
    operation: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LaneRequest {
    operation: String,
    lane_id: String,
    owner_id: String,
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum ProjectExecutionHostRequest {
    Submit {
        project_root: String,
        capability_id: String,
        idempotency_key: String,
        input: Value,
        #[serde(default)]
        priority: Option<crate::ProjectExecutionPriority>,
    },
    Status {
        job_id: String,
    },
    Cancel {
        job_id: String,
    },
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NeutralLlmRequest {
    #[serde(default)]
    pub(super) provider_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    conversation_id: Option<String>,
    #[serde(default)]
    llm_session_id: Option<String>,
    messages: Vec<LlmMessage>,
    mcp_servers: Vec<LlmMcpServerConfig>,
    #[serde(default)]
    options: LlmRequestOptions,
}

impl NeutralLlmRequest {
    pub(super) fn validate(&self) -> Result<(), HostCapabilityError> {
        if self.messages.len() > MAX_LLM_MESSAGES {
            return Err(quota(NEURAL_LLM, self.messages.len(), MAX_LLM_MESSAGES));
        }
        if self.mcp_servers.len() > MAX_LLM_SERVERS {
            return Err(quota(NEURAL_LLM, self.mcp_servers.len(), MAX_LLM_SERVERS));
        }
        let bytes = self
            .messages
            .iter()
            .map(|message| message.role.len() + message.content.len())
            .sum::<usize>();
        if bytes > MAX_LLM_TEXT_BYTES {
            return Err(quota(NEURAL_LLM, bytes, MAX_LLM_TEXT_BYTES));
        }
        for message in &self.messages {
            require_non_empty(NEURAL_LLM, &message.role, "message role")?;
            require_non_empty(NEURAL_LLM, &message.content, "message content")?;
        }
        if let Some(temperature) = self.options.temperature {
            if !(0.0..=2.0).contains(&temperature) {
                return Err(invalid(
                    NEURAL_LLM,
                    &json!(temperature),
                    "temperature within 0.0 through 2.0",
                ));
            }
        }
        if let Some(tokens) = self.options.max_output_tokens {
            if tokens == 0 || tokens > 32768 {
                return Err(invalid(
                    NEURAL_LLM,
                    &json!(tokens),
                    "max_output_tokens from 1 through 32768",
                ));
            }
        }
        if let Some(LlmResponseFormat::JsonSchema { name, schema, .. }) =
            self.options.response_format.as_ref()
        {
            if name.trim().is_empty() || !schema.is_object() {
                return Err(invalid(
                    NEURAL_LLM,
                    &json!(self.options.response_format),
                    "json_schema format with non-empty name and object schema",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn into_neural(self) -> LlmRequest {
        LlmRequest {
            options: self.options,
            messages: self.messages,
            stream: false,
            provider_id: self.provider_id,
            model: self.model,
            conversation_id: self.conversation_id,
            provider_session_id: self.llm_session_id,
            mcp_servers: self.mcp_servers,
            modality_inputs: Vec::new(),
        }
    }
}

fn exact_object<'a>(
    capability: &str,
    input: &'a Value,
) -> Result<&'a Map<String, Value>, HostCapabilityError> {
    input
        .as_object()
        .ok_or_else(|| invalid(capability, input, "JSON object"))
}
fn required<'a>(
    fields: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a Value, HostCapabilityError> {
    fields
        .get(key)
        .ok_or_else(|| invalid("host", &Value::Null, &format!("required field {key}")))
}
fn required_string<'a>(
    capability: &str,
    fields: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a str, HostCapabilityError> {
    fields
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            invalid(
                capability,
                fields.get(key).unwrap_or(&Value::Null),
                &format!("non-empty string field {key}"),
            )
        })
}

/// Optional canvas id defaulting to [`MAIN_CANVAS_ID`]; present values must
/// be non-empty strings.
fn optional_canvas_id<'a>(
    capability: &str,
    fields: &'a Map<String, Value>,
) -> Result<&'a str, HostCapabilityError> {
    match fields.get("canvas_id") {
        None | Some(Value::Null) => Ok(MAIN_CANVAS_ID),
        Some(value) => value
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| invalid(capability, value, "non-empty string field canvas_id")),
    }
}
fn exact_fields(
    capability: &str,
    fields: &Map<String, Value>,
    allowed: &[&str],
) -> Result<(), HostCapabilityError> {
    let unexpected = fields
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if unexpected.is_empty() {
        Ok(())
    } else {
        Err(invalid(
            capability,
            &json!(unexpected),
            &format!("only fields {}", allowed.join(", ")),
        ))
    }
}
fn decode<T: for<'de> Deserialize<'de>>(
    capability: &str,
    input: Value,
) -> Result<T, HostCapabilityError> {
    serde_json::from_value(input.clone())
        .map_err(|error| invalid(capability, &input, &error.to_string()))
}
fn optional_digit(payload: &Value) -> Result<u8, HostCapabilityError> {
    let digit = payload.get("digit").and_then(Value::as_u64).unwrap_or(3);
    u8::try_from(digit)
        .ok()
        .filter(|value| (1..=9).contains(value))
        .ok_or_else(|| {
            invalid(
                FRONTEND_ACTION,
                &json!(digit),
                "countdown digit from 1 through 9",
            )
        })
}
fn parse_orb_mode(payload: &Value) -> Result<OrbMode, HostCapabilityError> {
    match payload.get("mode").and_then(Value::as_str) {
        Some("idle") => Ok(OrbMode::Idle),
        Some("session") => Ok(OrbMode::Session),
        Some("activity") => Ok(OrbMode::Activity),
        Some("error") => Ok(OrbMode::Error),
        other => Err(invalid(
            FRONTEND_ACTION,
            &json!(other),
            "orb mode idle, session, activity, or error",
        )),
    }
}
fn resolve_llm_selection(
    frontend: &Mutex<FrontendCore>,
    capability: &str,
    request: &mut NeutralLlmRequest,
) -> Result<String, HostCapabilityError> {
    apply_configured_llm_selection(frontend, capability, request)?
        .ok_or_else(|| unavailable(capability, "no provider_id was selected"))
}
/// Fills `provider_id`/`model` from the app's configured assistant engine
/// when the caller left them unset. Returns `None` (never errors on a
/// missing provider) when neither an explicit nor a configured provider is
/// available, so callers that can defer resolution to later execution
/// (`runtime.background_job` queues a job before the provider ever runs)
/// accept an unresolved request as-is; `resolve_llm_selection` above layers
/// the immediate hard error synchronous callers (`neural_llm`) need.
fn apply_configured_llm_selection(
    frontend: &Mutex<FrontendCore>,
    capability: &str,
    request: &mut NeutralLlmRequest,
) -> Result<Option<String>, HostCapabilityError> {
    let explicit_provider = non_empty_option(request.provider_id.as_deref()).map(str::to_owned);
    let (configured_provider, configured_model) = configured_llm_selection(frontend, capability)?;
    let provider_id = explicit_provider.clone().or(configured_provider.clone());
    request.provider_id = provider_id.clone();
    if non_empty_option(request.model.as_deref()).is_none()
        && explicit_provider
            .as_ref()
            .is_none_or(|selected| Some(selected) == configured_provider.as_ref())
    {
        request.model = configured_model;
    }
    Ok(provider_id)
}
fn configured_llm_selection(
    frontend: &Mutex<FrontendCore>,
    capability: &str,
) -> Result<(Option<String>, Option<String>), HostCapabilityError> {
    let frontend = frontend
        .lock()
        .map_err(|_| failed(capability, "frontend mutex poisoned"))?;
    let settings = frontend.app_settings();
    let provider = (settings.assistant_engine != AssistantEngine::NativeMcp)
        .then(|| settings.assistant_engine.value().to_owned());
    let model = non_empty_option(settings.assistant_model.as_deref()).map(str::to_owned);
    Ok((provider, model))
}
fn non_empty_option(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
fn require_non_empty(
    capability: &str,
    value: &str,
    field: &str,
) -> Result<(), HostCapabilityError> {
    if value.trim().is_empty() {
        Err(invalid(
            capability,
            &json!(value),
            &format!("non-empty {field}"),
        ))
    } else {
        Ok(())
    }
}

fn invalid(capability: &str, value: &Value, expected: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        capability,
        "invalid_host_capability_input",
        format!("invalid {capability} input `{value}`; expected {expected}"),
        false,
    )
}
fn unavailable(capability: &str, message: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        capability,
        "host_capability_unavailable",
        format!("{capability} unavailable: {message}"),
        true,
    )
}
fn failed(capability: &str, message: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        capability,
        "host_capability_execution_failed",
        format!("{capability} operation failed: {message}"),
        false,
    )
}
fn quota(capability: &str, actual: usize, maximum: usize) -> HostCapabilityError {
    HostCapabilityError::new(
        capability,
        "host_capability_quota_exceeded",
        format!("{capability} quota exceeded: actual {actual}; maximum {maximum}"),
        false,
    )
}
fn unknown(capability: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        capability,
        "unknown_host_capability",
        format!("unknown Host Capability `{capability}`"),
        false,
    )
}

fn bounded_output(
    capability: &str,
    output: Value,
    maximum: usize,
) -> Result<Value, HostCapabilityError> {
    let actual = serde_json::to_vec(&output)
        .map_err(|error| failed(capability, &error.to_string()))?
        .len();
    if actual > maximum {
        return Err(quota(capability, actual, maximum));
    }
    Ok(output)
}

fn bounded_input(
    capability: &str,
    input: &Value,
    maximum: usize,
) -> Result<(), HostCapabilityError> {
    let actual = serde_json::to_vec(input)
        .map_err(|error| failed(capability, &error.to_string()))?
        .len();
    if actual > maximum {
        return Err(quota(capability, actual, maximum));
    }
    Ok(())
}

#[cfg(test)]
#[path = "plugin_host_capabilities/tests.rs"]
mod tests;
