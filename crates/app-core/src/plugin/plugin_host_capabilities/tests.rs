use super::*;
#[path = "tests/document_conversion.rs"]
mod document_conversion;
use lumvise_frontend_core::{AppSettingsPatch, AssistantEngine};
use lumvise_neural_core::llm_providers::contract::{LlmProvider, LlmStreamEventSink};
use lumvise_neural_core::llm_providers::{
    LlmCapabilitySupport, LlmProviderCapabilities, LlmResponse,
};
use lumvise_neural_core::process::StreamControl;
use std::sync::mpsc::{self, Receiver, Sender};

use crate::plugin::host_capability_catalog::{HostCapabilityRoute, host_capability_definitions};
use lumvise_neural_core::text2voice::{Text2VoiceResponse, Text2VoiceStreamEventSink};
use lumvise_neural_core::types::EngineMetadata;

struct ConfiguredAssistantProvider;

impl LlmProvider for ConfiguredAssistantProvider {
    fn provider_id(&self) -> &str {
        "cerebras"
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        LlmProviderCapabilities {
            provider_id: self.provider_id().into(),
            final_text_output: LlmCapabilitySupport::Supported,
            streamed_text_output: LlmCapabilitySupport::Supported,
            image_snapshot_input: LlmCapabilitySupport::Unsupported,
            live_audio_input: LlmCapabilitySupport::Unsupported,
            screen_frame_broadcast_input: LlmCapabilitySupport::Unsupported,
            native_audio_output: LlmCapabilitySupport::Unsupported,
        }
    }

    fn complete(&self, request: &LlmRequest) -> lumvise_neural_core::Result<LlmResponse> {
        Ok(LlmResponse {
            provider_id: self.provider_id().into(),
            model: request.model.clone().unwrap_or_default(),
            content: "configured provider response".into(),
            metadata: json!({}),
        })
    }

    fn stream_with_events(
        &self,
        _request: &LlmRequest,
        _control: StreamControl,
        _on_event: &mut LlmStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        Ok(())
    }
}

/// Installed-but-never-invoked recognizer: `speech_availability` reports only
/// whether the slot is filled, so the double needs no working transcription.
struct InstalledRecognizer;

impl SpeechRecognizer for InstalledRecognizer {
    fn warmup(&self) -> lumvise_neural_core::Result<()> {
        Ok(())
    }

    fn transcribe(
        &self,
        _request: &lumvise_neural_core::voice2text::Voice2TextRequest,
        _control: &lumvise_resource_routing::InvocationControl,
    ) -> lumvise_neural_core::Result<lumvise_neural_core::voice2text::Voice2TextResponse> {
        Err(lumvise_neural_core::error::NeuralError::ProviderFailed {
            provider_id: "installed-recognizer".into(),
            message: "availability probe must not transcribe".into(),
        })
    }

    fn stream_with_events(
        &self,
        _request: &lumvise_neural_core::voice2text::Voice2TextRequest,
        _control: &lumvise_resource_routing::InvocationControl,
        _on_event: &mut lumvise_neural_core::voice2text::Voice2TextStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        Err(lumvise_neural_core::error::NeuralError::ProviderFailed {
            provider_id: "installed-recognizer".into(),
            message: "availability probe must not stream".into(),
        })
    }
}

struct StreamingSynthesizer;

impl SpeechSynthesizer for StreamingSynthesizer {
    fn warmup(&self) -> lumvise_neural_core::Result<()> {
        Ok(())
    }

    fn synthesize(
        &self,
        _request: &Text2VoiceRequest,
        _control: &InvocationControl,
    ) -> lumvise_neural_core::Result<Text2VoiceResponse> {
        Ok(Text2VoiceResponse {
            audio: vec![1, 2],
            media_type: "audio/pcm;rate=24000;format=s16le".into(),
            sample_rate_hz: Some(24_000),
            metadata: EngineMetadata {
                engine_id: "streaming-test".into(),
                model: None,
                metadata: Value::Null,
            },
        })
    }

    fn stream_with_events(
        &self,
        _request: &Text2VoiceRequest,
        _control: &InvocationControl,
        on_event: &mut Text2VoiceStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        on_event(Text2VoiceStreamEvent::AudioChunk {
            sequence: 9,
            audio: vec![1, 2],
            media_type: "audio/pcm;rate=24000;format=s16le".into(),
        })?;
        on_event(Text2VoiceStreamEvent::Complete)
    }
}

/// Records dispatched requests so neutral pass-through behavior is provable.
struct RecordingLlmProvider {
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl LlmProvider for RecordingLlmProvider {
    fn provider_id(&self) -> &str {
        "cerebras"
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        ConfiguredAssistantProvider.capabilities()
    }

    fn complete(&self, request: &LlmRequest) -> lumvise_neural_core::Result<LlmResponse> {
        self.requests
            .lock()
            .expect("provider requests")
            .push(request.clone());
        Ok(LlmResponse {
            provider_id: self.provider_id().into(),
            model: request.model.clone().unwrap_or_default(),
            content: "{}".into(),
            metadata: json!({}),
        })
    }

    fn stream_with_events(
        &self,
        _request: &LlmRequest,
        _control: StreamControl,
        _on_event: &mut LlmStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        Ok(())
    }
}

fn services() -> Arc<PluginHostServices> {
    let mut frontend = FrontendCore::default();
    frontend
        .spawn_app(lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0))
        .expect("spawn test frontend");
    PluginHostServices::new(frontend, LlmProviderRegistry::empty(), test_persistence())
}

/// In-memory semantic persistence shared with the canvas file storage seam.
pub(crate) fn test_persistence() -> Arc<dyn lumvise_db_core::SemanticPersistence> {
    Arc::new(lumvise_db_core::LocalPersistence::in_memory().expect("test semantic persistence"))
}

struct HeldLaneProvider {
    entered: Sender<String>,
    release: Mutex<Receiver<()>>,
}

impl LlmProvider for HeldLaneProvider {
    fn provider_id(&self) -> &str {
        "lane-test"
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        LlmProviderCapabilities {
            provider_id: self.provider_id().into(),
            final_text_output: LlmCapabilitySupport::Supported,
            streamed_text_output: LlmCapabilitySupport::Unsupported,
            image_snapshot_input: LlmCapabilitySupport::Unsupported,
            live_audio_input: LlmCapabilitySupport::Unsupported,
            screen_frame_broadcast_input: LlmCapabilitySupport::Unsupported,
            native_audio_output: LlmCapabilitySupport::Unsupported,
        }
    }

    fn complete(&self, request: &LlmRequest) -> lumvise_neural_core::Result<LlmResponse> {
        let content = request.messages[0].content.clone();
        self.entered
            .send(content.clone())
            .expect("provider entry sent");
        if content == "hold" {
            self.release
                .lock()
                .expect("release receiver")
                .recv_timeout(Duration::from_secs(10))
                .expect("ordinary call released");
        }
        Ok(LlmResponse {
            provider_id: self.provider_id().into(),
            model: String::new(),
            content,
            metadata: json!({}),
        })
    }

    fn stream_with_events(
        &self,
        _request: &LlmRequest,
        _control: StreamControl,
        _on_event: &mut LlmStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        unreachable!("streaming is not under test")
    }
}

fn lane_request(content: &str) -> Value {
    json!({"provider_id": "lane-test", "model": null, "conversation_id": null,
        "llm_session_id": null, "messages": [{"role": "user", "content": content}],
        "mcp_servers": []})
}

fn held_lane_services() -> (Arc<PluginHostServices>, Receiver<String>, Sender<()>) {
    let (entered, entries) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let providers =
        LlmProviderRegistry::from_provider_instances(vec![Box::new(HeldLaneProvider {
            entered,
            release: Mutex::new(held),
        })])
        .expect("lane provider registry");
    (
        PluginHostServices::new(FrontendCore::default(), providers, test_persistence()),
        entries,
        release,
    )
}

fn start_ordinary_lane_call(
    services: &Arc<PluginHostServices>,
    content: &'static str,
) -> std::thread::JoinHandle<Result<Value, HostCapabilityError>> {
    let ordinary = Arc::clone(&services);
    std::thread::spawn(move || ordinary.invoke("plugin.other", NEURAL_LLM, lane_request(content)))
}

fn expect_lane_entry(entries: &Receiver<String>, content: &str) {
    assert_eq!(
        entries.recv_timeout(Duration::from_secs(2)).unwrap(),
        content
    );
}

fn assert_assistant_direct_lane(services: &PluginHostServices, entries: &Receiver<String>) {
    let direct = services
        .invoke("builtin.assistant", NEURAL_LLM, lane_request("direct"))
        .expect("Assistant direct completion");
    assert_eq!(direct["response"]["content"], "direct");
    expect_lane_entry(entries, "direct");
}

fn assert_assistant_controlled_lane(services: &PluginHostServices, entries: &Receiver<String>) {
    use lumvise_plugin_runtime::PluginInvocationClass;

    let context = PluginInvocationContext::new(
        "assistant-controlled",
        "builtin.assistant",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(5),
    );
    let controlled = services
        .invoke_controlled(
            "builtin.assistant",
            NEURAL_LLM,
            lane_request("controlled"),
            &context,
        )
        .expect("Assistant controlled completion");
    assert_eq!(controlled["response"]["content"], "controlled");
    expect_lane_entry(entries, "controlled");
}

fn assert_assistant_background_lane(services: &PluginHostServices, entries: &Receiver<String>) {
    let accepted = services
        .invoke(
            "builtin.assistant",
            BACKGROUND_JOB,
            json!({
                "operation": "accept_llm", "job_kind": "lane", "request": lane_request("background")
            }),
        )
        .expect("Assistant job accepted");
    let finished = services
        .invoke(
            "builtin.assistant",
            BACKGROUND_JOB,
            json!({
                "operation": "wait", "job_id": accepted["job_id"], "timeout_ms": 2000
            }),
        )
        .expect("Assistant job completed");
    assert_eq!(finished["status"], "completed");
    assert_eq!(finished["response"]["content"], "background");
    expect_lane_entry(entries, "background");
}

#[test]
fn assistant_calls_use_independent_provider_workers() {
    let (services, entries, release) = held_lane_services();
    let holding = start_ordinary_lane_call(&services, "hold");
    expect_lane_entry(&entries, "hold");
    let queued = start_ordinary_lane_call(&services, "queued");

    assert_assistant_direct_lane(&services, &entries);
    assert_assistant_controlled_lane(&services, &entries);
    assert_assistant_background_lane(&services, &entries);
    assert_lane_activity(&services);
    assert!(
        entries.try_recv().is_err(),
        "other plugin entered the held ordinary worker"
    );
    assert!(!queued.is_finished(), "ordinary call completed while held");

    release.send(()).expect("release ordinary worker");
    holding
        .join()
        .expect("holding call thread")
        .expect("holding call");
    queued
        .join()
        .expect("queued call thread")
        .expect("queued call");
    expect_lane_entry(&entries, "queued");
}

fn assert_lane_activity(services: &PluginHostServices) {
    use crate::workspace_activity::{ActivityKind, ActivityStatus};
    let activity = services.activity();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut cursor = None;
    loop {
        let snapshot = activity.wait_for_changes("/repo", cursor, Duration::from_millis(50));
        cursor = Some(snapshot.revision);
        if let Some(entries) = snapshot.entries {
            let queued = entries
                .iter()
                .filter(|entry| entry.status == ActivityStatus::Queued)
                .count();
            if queued == 1 {
                assert_eq!(
                    entries
                        .iter()
                        .filter(|entry| entry.status == ActivityStatus::Running)
                        .count(),
                    1
                );
                assert_eq!(
                    entries
                        .iter()
                        .filter(|entry| entry.kind == ActivityKind::Assistant
                            && entry.status == ActivityStatus::Succeeded)
                        .count(),
                    3
                );
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "ordinary queued activity was not published"
        );
    }
}

#[test]
fn host_owned_versions_publish_the_complete_neutral_contract() {
    let host = crate::plugin::host_capability_catalog::compiled_host_capability_versions();
    let identities = host.keys().copied().collect::<Vec<_>>();

    assert_eq!(
        identities,
        vec![
            "frontend.action",
            "frontend.canvas",
            "modalities.speech_to_text",
            "modalities.text_to_speech",
            "neural.embed",
            "neural.llm",
            "plugin.invoke",
            "project.source",
            "runtime.assistant_engine_availability",
            "runtime.audio_session",
            "runtime.background_job",
            "runtime.document_conversion_options",
            "runtime.exclusive_lane",
            "runtime.project_execution",
            "runtime.scoped_mcp",
            "runtime.speech_availability",
            "runtime.turn_wait",
            "semantic.snapshot",
            "storage.plugin",
            "storage.semantic",
        ]
    );
    assert_eq!(host["storage.semantic"], semver::Version::new(1, 4, 0));
    assert!(
        host.iter()
            .filter(|(id, _)| **id != "storage.semantic")
            .all(|(_, version)| version == &semver::Version::new(1, 0, 0))
    );
}

#[test]
fn every_cataloged_service_capability_reaches_an_adapter() {
    let services = services();

    for definition in host_capability_definitions().iter().filter(|definition| {
        // Parameterless queries have no fields to validate — see
        // `assistant_engine_availability_accepts_empty_input` and
        // `speech_availability_names_each_missing_dependency` below.
        definition.route == HostCapabilityRoute::Services
            && definition.id != ASSISTANT_ENGINE_AVAILABILITY
            && definition.id != SPEECH_AVAILABILITY
            && definition.id != crate::plugin::host_capability_catalog::DOCUMENT_CONVERSION_OPTIONS
    }) {
        let error = services
            .invoke("plugin.test", definition.id, json!({}))
            .expect_err("empty input must be rejected by the owning adapter");

        assert!(
            !error.to_string().contains("unknown_host_capability"),
            "cataloged service `{}` has no adapter",
            definition.id
        );
    }
}

#[test]
fn assistant_engine_availability_accepts_empty_input() {
    let services = services();

    let result = services
        .invoke("plugin.test", ASSISTANT_ENGINE_AVAILABILITY, json!({}))
        .expect("parameterless capability accepts empty input");

    assert_eq!(result["available"], false);
}

/// Issue #92: both speech dependencies must be startup-detectable, and a
/// missing one must be named so a caller can say which piece to configure.
#[test]
fn speech_availability_names_each_missing_dependency() {
    let services = services();

    let result = services
        .invoke("plugin.test", SPEECH_AVAILABILITY, json!({}))
        .expect("parameterless capability accepts empty input");

    assert_eq!(result["available"], false);
    assert_eq!(result["speech_to_text"], false);
    assert_eq!(result["text_to_speech"], false);
    assert_eq!(
        result["missing"],
        json!(["Speech-to-Text model", "Text-to-Speech voice"])
    );
}

#[test]
fn speech_availability_names_only_the_missing_synthesizer() {
    let services = services();
    services.install_speech_recognizer(Arc::new(InstalledRecognizer));

    let result = services
        .invoke("plugin.test", SPEECH_AVAILABILITY, json!({}))
        .expect("availability probe succeeds");

    assert_eq!(result["available"], false);
    assert_eq!(result["speech_to_text"], true);
    assert_eq!(result["missing"], json!(["Text-to-Speech voice"]));
}

#[test]
fn speech_availability_names_only_the_missing_recognizer() {
    let services = services();
    services.install_speech_synthesizer(Arc::new(StreamingSynthesizer));

    let result = services
        .invoke("plugin.test", SPEECH_AVAILABILITY, json!({}))
        .expect("availability probe succeeds");

    assert_eq!(result["available"], false);
    assert_eq!(result["text_to_speech"], true);
    assert_eq!(result["missing"], json!(["Speech-to-Text model"]));
}

#[test]
fn speech_availability_reports_ready_when_both_are_installed() {
    let services = services();
    services.install_speech_recognizer(Arc::new(InstalledRecognizer));
    services.install_speech_synthesizer(Arc::new(StreamingSynthesizer));

    let result = services
        .invoke("plugin.test", SPEECH_AVAILABILITY, json!({}))
        .expect("availability probe succeeds");

    assert_eq!(result["available"], true);
    assert_eq!(result["missing"], json!([]));
}

#[test]
fn speech_preferences_gate_installed_services_independently() {
    let app = crate::AppCore::in_memory()
        .unwrap()
        .with_speech_recognizer(Arc::new(InstalledRecognizer))
        .with_speech_synthesizer(Arc::new(StreamingSynthesizer));
    for (input, output) in [(false, true), (true, false), (false, false), (true, true)] {
        app.frontend()
            .apply_app_settings_patch(&AppSettingsPatch::SpeechRecognitionEnabled(input))
            .unwrap();
        app.frontend()
            .apply_app_settings_patch(&AppSettingsPatch::SpeechSynthesisEnabled(output))
            .unwrap();
        let availability = app
            .plugin_host_services
            .invoke("plugin.test", SPEECH_AVAILABILITY, json!({}))
            .unwrap();
        assert_eq!(availability["speech_to_text"], input);
        assert_eq!(availability["text_to_speech"], output);
        assert_eq!(app.voice2text_service().is_some(), input);
        assert_eq!(app.text2voice_service().is_some(), output);
    }
}

#[test]
fn disabled_synthesis_refuses_playback_before_creating_a_segment() {
    let services = services();
    services.install_speech_synthesizer(Arc::new(StreamingSynthesizer));
    services
        .frontend
        .lock()
        .unwrap()
        .apply_app_settings_patch(&AppSettingsPatch::SpeechSynthesisEnabled(false));
    let error = services.invoke("plugin.test", TEXT_TO_SPEECH, json!({
        "playback_id": "setup-disabled", "session_id": "setup-test", "text": "Hello", "close": true
    })).unwrap_err();
    assert!(error.to_string().contains("disabled"));
    assert!(services.playback_owners.lock().unwrap().is_empty());
}

#[test]
fn project_execution_adapter_derives_requester_from_invoking_plugin() {
    let services = services();
    let project_execution = crate::ProjectExecutionService::default();
    let (commands, receiver) = std::sync::mpsc::sync_channel(32);
    project_execution
        .register_provider(
            "mcp-1",
            "/work/project",
            ["semantic.generate_functional_artifacts.v1".into()],
            commands,
        )
        .expect("register provider");
    services.install_project_execution(project_execution);

    let job = services
        .invoke(
            "builtin.knowledge",
            PROJECT_EXECUTION,
            json!({
                "operation": "submit",
                "project_root": "/work/project",
                "capability_id": "semantic.generate_functional_artifacts.v1",
                "idempotency_key": "artifact:fn:parse",
                "input": {"semantic_element_id": "fn:parse"}
            }),
        )
        .expect("submit through Host Capability");

    assert_eq!(job["requester_id"], "builtin.knowledge");
    assert!(matches!(
        receiver.recv().expect("provider command"),
        crate::ProjectExecutionCommand::Execute { .. }
    ));
}

#[test]
fn frontend_and_canvas_adapters_mutate_shared_host_state() {
    let services = services();
    services
        .invoke(
            "plugin.test",
            FRONTEND_ACTION,
            json!({"action": "frontend.set_orb_mode", "payload": {"mode": "activity"}}),
        )
        .expect("frontend action");
    let canvas = services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({"operation": "update", "patch": {"canvas_id": "main", "elements": []}}),
        )
        .expect("canvas update");
    assert_eq!(canvas["revision"], 1);
    assert_eq!(
        services.frontend.lock().expect("frontend").state().orb.mode,
        OrbMode::Activity
    );
}

#[test]
fn plugin_defined_frontend_action_is_queued_without_core_interpretation() {
    let services = services();

    let result = services
        .invoke(
            "plugin.test",
            FRONTEND_ACTION,
            json!({
                "action": "plugin.test.sync_canvas",
                "payload": {"canvas": {"revision": 2}}
            }),
        )
        .expect("plugin-defined frontend action");

    assert_eq!(result, json!({"accepted": true}));
    assert_eq!(
        services
            .frontend_actions
            .lock()
            .expect("frontend actions")
            .as_slice(),
        [json!({
            "action": "plugin.test.sync_canvas",
            "payload": {"canvas": {"revision": 2}}
        })]
    );
}

#[test]
fn frontend_action_id_is_applied_and_queued_exactly_once() {
    let services = services();
    let action = json!({
        "action_id": "assistant:session-1:1:start",
        "action": "frontend.start_countdown",
        "payload": {"digit": 3}
    });

    services
        .invoke("plugin.test", FRONTEND_ACTION, action.clone())
        .expect("first frontend action");
    let duplicate = services
        .invoke("plugin.test", FRONTEND_ACTION, action)
        .expect("duplicate frontend action");

    assert_eq!(duplicate, json!({"accepted": true, "duplicate": true}));
    assert_eq!(
        services
            .frontend_actions
            .lock()
            .expect("frontend actions")
            .len(),
        1
    );
}

#[test]
fn assistant_preparation_is_queued_without_starting_native_countdown() {
    let services = services();
    let initial_orb = services
        .frontend
        .lock()
        .expect("frontend")
        .state()
        .orb
        .clone();

    let result = services
        .invoke(
            "builtin.assistant",
            FRONTEND_ACTION,
            json!({
                "action": "frontend.prepare_assistant_session",
                "payload": {
                    "session_id": "prepared-session",
                    "digit": 3,
                    "provider_job_id": "job-1"
                }
            }),
        )
        .expect("prepare Assistant session");

    assert_eq!(result, json!({"accepted": true, "deferred": true}));
    assert_eq!(
        services.frontend.lock().expect("frontend").state().orb,
        initial_orb
    );
}

#[test]
fn malformed_and_missing_services_fail_with_stable_codes() {
    let services = services();
    let malformed = services
        .invoke(
            "plugin.test",
            SCOPED_MCP,
            json!({"operation": "base_url", "extra": true}),
        )
        .expect_err("unknown field rejected");
    let unavailable = services.invoke("plugin.test", NEURAL_LLM, json!({"provider_id": null, "model": null, "conversation_id": null, "llm_session_id": null, "messages": [{"role": "user", "content": "hello"}], "mcp_servers": []})).expect_err("missing provider rejected");
    assert!(
        malformed
            .to_string()
            .contains("invalid_host_capability_input")
    );
    assert!(
        unavailable
            .to_string()
            .contains("host_capability_unavailable")
    );
}

#[test]
fn missing_llm_selection_uses_the_frontend_assistant_provider_and_model() {
    let mut frontend = FrontendCore::default();
    frontend.apply_app_settings_patch(&AppSettingsPatch::AssistantEngine(
        AssistantEngine::Cerebras,
    ));
    frontend.apply_app_settings_patch(&AppSettingsPatch::AssistantModel(Some(
        "configured-model".into(),
    )));
    let registry =
        LlmProviderRegistry::from_provider_instances(vec![Box::new(ConfiguredAssistantProvider)])
            .expect("configured provider registry");
    let services = PluginHostServices::new(frontend, registry, test_persistence());

    let output = services
        .invoke(
            "builtin.assistant",
            NEURAL_LLM,
            json!({
                "provider_id": null,
                "model": null,
                "conversation_id": null,
                "llm_session_id": null,
                "messages": [{"role": "user", "content": "hello"}],
                "mcp_servers": []
            }),
        )
        .expect("frontend-configured LLM selection");

    assert_eq!(output["response"]["provider_id"], "cerebras");
    assert_eq!(output["response"]["model"], "configured-model");
}

#[test]
fn mcp_lane_and_speech_adapters_are_neutral_and_fail_closed() {
    let services = services();
    *services.scoped_mcp_base_url.lock().expect("MCP URL") = Some("http://127.0.0.1:7777".into());
    let mcp = services
        .invoke("plugin.test", SCOPED_MCP, json!({"operation": "base_url"}))
        .expect("MCP URL");
    let lane = services
        .invoke(
            "plugin.test",
            EXCLUSIVE_LANE,
            json!({"operation": "snapshot", "lane_id": "assistant", "owner_id": "owner", "session_id": null}),
        )
        .expect("lane snapshot");
    let speech = services
        .invoke(
            "plugin.test",
            TEXT_TO_SPEECH,
            json!({"playback_id": "p1", "session_id": "s1", "text": "hello", "voice_id": null, "model": null}),
        )
        .expect_err("missing TTS rejected");
    assert_eq!(mcp["base_url"], "http://127.0.0.1:7777");
    assert_eq!(lane["queued"], 0);
    assert!(speech.to_string().contains("host_capability_unavailable"));
}

#[test]
fn text_to_speech_streams_ordered_audio_to_the_desktop_transport() {
    let services = services();
    services.install_speech_synthesizer(Arc::new(StreamingSynthesizer));
    let receiver = services.voice_playback_transport().subscribe();

    let accepted = services
        .invoke(
            "builtin.assistant",
            TEXT_TO_SPEECH,
            json!({
                "playback_id": "streamed-p1", "session_id": "session-1",
                "text": "A complete sentence.", "close": true
            }),
        )
        .expect("streaming synthesis accepted");

    assert_eq!(accepted["accepted"], true);
    assert_eq!(accepted["segment_index"], 0);
    let events = (0..3)
        .map(|_| receiver.recv_timeout(Duration::from_secs(1)).unwrap())
        .collect::<Vec<_>>();
    assert!(matches!(
        events[0],
        VoicePlaybackTransportEvent::Opened { .. }
    ));
    assert!(matches!(
        events[1],
        VoicePlaybackTransportEvent::AudioChunk { sequence: 0, .. }
    ));
    assert!(matches!(
        events[2],
        VoicePlaybackTransportEvent::Closed { .. }
    ));
}

#[test]
fn stale_playback_status_reports_are_lifecycle_noise_not_failures() {
    let services = services();
    services.install_speech_synthesizer(Arc::new(StreamingSynthesizer));

    // Turn 1 occupies the single active playback slot...
    services
        .invoke(
            "builtin.assistant",
            TEXT_TO_SPEECH,
            json!({"playback_id": "turn-1", "session_id": "session-1", "text": "first"}),
        )
        .expect("first turn accepted");
    // ...and turn 2 replaces it before the renderer finishes turn 1.
    services
        .invoke(
            "builtin.assistant",
            TEXT_TO_SPEECH,
            json!({"playback_id": "turn-2", "session_id": "session-1", "text": "second"}),
        )
        .expect("second turn accepted");

    // The renderer's late report for the replaced turn must not fail: it used
    // to surface as a bogus session last_error ("expected active voice
    // playback id") and degrade the live session.
    let stale = services
        .report_voice_playback_status("turn-1", VoicePlaybackStatus::Completed)
        .expect("stale report is lifecycle noise, not a failure");
    assert_eq!(stale, None);

    // The active turn's own report still drives the session forward.
    let active = services
        .report_voice_playback_status("turn-2", VoicePlaybackStatus::Completed)
        .expect("active report applied");
    assert_eq!(
        active.map(|(plugin_id, _, _)| plugin_id).as_deref(),
        Some("builtin.assistant")
    );
}

#[test]
fn llm_and_text_quotas_reject_unbounded_work() {
    let services = services();
    let messages = (0..=MAX_LLM_MESSAGES)
        .map(|_| json!({"role": "user", "content": "x"}))
        .collect::<Vec<_>>();
    let llm = services.invoke("plugin.test", NEURAL_LLM, json!({"provider_id": "fake", "model": null, "conversation_id": null, "llm_session_id": null, "messages": messages, "mcp_servers": []})).expect_err("LLM quota");
    let tts = services.invoke("plugin.test", TEXT_TO_SPEECH, json!({"playback_id": "p1", "session_id": "s1", "text": "x".repeat(MAX_TTS_TEXT_BYTES + 1), "voice_id": null, "model": null})).expect_err("TTS quota");
    assert!(llm.to_string().contains("host_capability_quota_exceeded"));
    assert!(tts.to_string().contains("host_capability_quota_exceeded"));
}

#[test]
fn canvas_input_quota_rejects_unbounded_patch_before_decode() {
    let services = services();
    let error = services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({"operation": "update", "patch": {"padding": "x".repeat(MAX_CANVAS_INPUT_BYTES)}}),
        )
        .expect_err("canvas quota");

    assert!(error.to_string().contains("host_capability_quota_exceeded"));
}

#[test]
fn canvas_diff_provenance_is_derived_from_the_calling_plugin() {
    let services = services();
    let result = services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({"operation": "apply_diff", "canvas_id": "main", "patch": []}),
        )
        .expect("canvas diff");
    let spoof = services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({"operation": "apply_diff", "canvas_id": "main", "patch": [], "source": "user"}),
        )
        .expect_err("caller-controlled provenance rejected");

    assert_eq!(result["last_updated_by"], "plugin:plugin.test");
    assert!(spoof.to_string().contains("invalid_host_capability_input"));
}

#[test]
fn project_execution_capacity_errors_map_to_quota_exceeded() {
    let capacity = super::project_execution_error(crate::ProjectExecutionError::CapacityExceeded {
        actual: 5,
        maximum: 4,
    });
    assert!(
        capacity
            .to_string()
            .contains("host_capability_quota_exceeded")
    );

    let queue = super::project_execution_error(crate::ProjectExecutionError::ProviderQueueFull {
        provider_id: "p1".into(),
        maximum: 8,
    });
    assert!(queue.to_string().contains("host_capability_quota_exceeded"));
}

#[test]
fn llm_options_pass_through_to_the_provider() {
    let mut frontend = FrontendCore::default();
    frontend.apply_app_settings_patch(&AppSettingsPatch::AssistantEngine(
        AssistantEngine::Cerebras,
    ));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let registry =
        LlmProviderRegistry::from_provider_instances(vec![Box::new(RecordingLlmProvider {
            requests: Arc::clone(&requests),
        })])
        .expect("recording provider registry");
    let services = PluginHostServices::new(frontend, registry, test_persistence());

    services
        .invoke(
            "builtin.assistant",
            NEURAL_LLM,
            json!({
                "provider_id": null,
                "model": null,
                "conversation_id": null,
                "llm_session_id": null,
                "messages": [{"role": "user", "content": "hello"}],
                "mcp_servers": [],
                "options": {
                    "max_output_tokens": 512,
                    "temperature": 0.7,
                    "reasoning_effort": "low",
                    "response_format": {
                        "type": "json_schema",
                        "name": "result",
                        "schema": {"type": "object"},
                        "strict": true
                    }
                }
            }),
        )
        .expect("LLM invocation with options");

    let recorded = requests.lock().expect("provider requests").clone();
    assert_eq!(recorded.len(), 1);
    let options = &recorded[0].options;
    assert_eq!(options.max_output_tokens, Some(512));
    assert_eq!(options.temperature, Some(0.7));
    assert_eq!(
        serde_json::to_value(options).unwrap()["reasoning_effort"],
        "low"
    );
    match &options.response_format {
        Some(LlmResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        }) => {
            assert_eq!(name, "result");
            assert_eq!(schema, &json!({"type": "object"}));
            assert!(strict);
        }
        other => panic!("expected json_schema response format: {other:?}"),
    }
}

#[test]
fn llm_options_are_validated_before_dispatch() {
    let services = services();
    for options in [
        json!({"reasoning_effort": "very-fast"}),
        json!({"temperature": 2.5}),
        json!({"temperature": -0.1}),
        json!({"max_output_tokens": 0}),
        json!({"max_output_tokens": 32769}),
        json!({"response_format": {"type": "json_schema", "name": "  ", "schema": {"type": "object"}, "strict": true}}),
        json!({"response_format": {"type": "json_schema", "name": "result", "schema": "not-an-object", "strict": true}}),
    ] {
        let error = services
            .invoke(
                "plugin.test",
                NEURAL_LLM,
                json!({
                    "provider_id": null,
                    "model": null,
                    "conversation_id": null,
                    "llm_session_id": null,
                    "messages": [{"role": "user", "content": "hello"}],
                    "mcp_servers": [],
                    "options": options
                }),
            )
            .expect_err("invalid options rejected");
        assert!(error.to_string().contains("invalid_host_capability_input"));
    }
}

#[test]
fn frontend_canvas_get_and_update_scope_by_canvas_id() {
    let services = services();
    let workspace = services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({
                "operation": "update",
                "canvas_id": "workspace:3f",
                "patch": {"canvas_id": "workspace:3f", "elements": []}
            }),
        )
        .expect("workspace canvas update");
    assert_eq!(workspace["revision"], 1);
    assert_eq!(workspace["canvas_id"], "workspace:3f");

    // Default get reads main, which the workspace update never touched.
    let main = services
        .invoke("plugin.test", FRONTEND_CANVAS, json!({"operation": "get"}))
        .expect("default get");
    assert_eq!(main["canvas_id"], "main");
    assert_eq!(main["revision"], 0);

    // Explicit canvas_id get reads the workspace canvas.
    let workspace_get = services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({"operation": "get", "canvas_id": "workspace:3f"}),
        )
        .expect("workspace get");
    assert_eq!(workspace_get["revision"], 1);

    // Default update targets main and stays independent of the workspace.
    let main_update = services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({"operation": "update", "patch": {"canvas_id": "main", "elements": []}}),
        )
        .expect("default update");
    assert_eq!(main_update["revision"], 1);
    let workspace_after = services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({"operation": "get", "canvas_id": "workspace:3f"}),
        )
        .expect("workspace get after main update");
    assert_eq!(workspace_after["revision"], 1);
}

#[test]
fn frontend_canvas_apply_diff_and_export_files_target_canvas() {
    let services = services();
    let file_entry = json!({
        "mimeType": "image/svg+xml",
        "dataURL": "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg'%3E%3C/svg%3E"
    });
    let diffed = services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({
                "operation": "apply_diff",
                "canvas_id": "workspace:3f",
                "patch": [{"op": "add", "path": "/files/ws-file", "value": file_entry}]
            }),
        )
        .expect("workspace apply_diff");
    assert_eq!(diffed["revision"], 1);

    let exported = services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({"operation": "export_files", "artifact_id": "artifact-1", "canvas_id": "workspace:3f"}),
        )
        .expect("workspace export_files");
    assert!(exported["files"].get("ws-file").is_some());

    // Default export reads main, which has no files.
    let default_export = services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({"operation": "export_files", "artifact_id": "artifact-1"}),
        )
        .expect("default export_files");
    assert_eq!(default_export["files"], json!({}));

    let main = services
        .invoke("plugin.test", FRONTEND_CANVAS, json!({"operation": "get"}))
        .expect("default get");
    assert_eq!(main["revision"], 0);
}

#[test]
fn frontend_canvas_discard_removes_canvas_and_rejects_main() {
    let services = services();
    services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({
                "operation": "update",
                "canvas_id": "workspace:3f",
                "patch": {"canvas_id": "workspace:3f", "elements": []}
            }),
        )
        .expect("workspace canvas update");

    let discarded = services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({"operation": "discard", "canvas_id": "workspace:3f"}),
        )
        .expect("discard existing canvas");
    assert_eq!(discarded, json!({"discarded": true}));

    let missing = services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({"operation": "discard", "canvas_id": "workspace:3f"}),
        )
        .expect("discard missing canvas");
    assert_eq!(missing, json!({"discarded": false}));

    let workspace = services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({"operation": "get", "canvas_id": "workspace:3f"}),
        )
        .expect("discarded canvas reads empty");
    assert_eq!(workspace["revision"], 0);

    let main_rejection = services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({"operation": "discard", "canvas_id": "main"}),
        )
        .expect_err("main canvas discard rejected");
    assert!(main_rejection.to_string().contains("non-main canvas id"));
}

#[test]
fn frontend_canvas_apply_diff_restores_native_document_and_image_files() {
    let services = services();
    let document = json!({"type":"excalidraw", "version":2, "elementsById":{},
        "elementOrder":[], "appState":{}, "files":{"image-1":{"dataURL":"data:image/png;base64,AA=="}}});
    services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({"operation":"apply_diff",
        "canvas_id":"workspace:t1", "patch":[{"op":"replace", "path":"", "value":document}]}),
        )
        .expect("restore conversation document");
    let restored = services
        .invoke(
            "plugin.test",
            FRONTEND_CANVAS,
            json!({"operation":"get", "canvas_id":"workspace:t1"}),
        )
        .expect("read restored canvas");
    assert_eq!(restored["document"], document);
    assert_eq!(restored["scene"]["files"], document["files"]);
}
