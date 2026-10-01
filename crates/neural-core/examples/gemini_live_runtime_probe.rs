use lumvise_neural_core::llm_providers::contract::LlmProvider;
use lumvise_neural_core::llm_providers::gemini::{
    GeminiProvider,
    live::{GeminiLiveTraceEvent, GeminiLiveTraceStage, GeminiLiveWebSocketTransport},
};
use lumvise_neural_core::llm_providers::{
    LlmMessage, LlmModalityInput, LlmModalityInputKind, LlmRequest, LlmStreamEvent,
};
use lumvise_neural_core::process::StreamControl;
use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, SpawnConfig};
use serde::Serialize;
use serde_json::{Value, json};
use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_LIVE_MODEL: &str = "gemini-2.5-flash-native-audio-preview-12-2025";

#[derive(Debug, Clone, Serialize)]
struct TraceRecord {
    elapsed_ms: u128,
    #[serde(flatten)]
    event: GeminiLiveTraceEvent,
}

fn main() -> Result<(), Box<dyn Error>> {
    let credential = gemini_live_credential()?;
    let model = env_first(&["LUMVISE_GEMINI_LIVE_MODEL", "LUMVISE_GEMINI_MODEL"])
        .unwrap_or_else(|| DEFAULT_LIVE_MODEL.to_string());
    let audio = synthetic_pcm16_audio(16_000, 700);
    let screen_frames = vec![one_pixel_png(), one_pixel_png()];
    let request = live_probe_request(&model, audio.clone(), screen_frames.clone());
    let trace = Arc::new(Mutex::new(Vec::new()));
    let provider = traced_provider(model, credential.value.clone(), trace.clone())?;
    let events = match stream_events(&provider, &request) {
        Ok(events) => events,
        Err(error) => {
            emit_failed_probe_report(&credential, &request, &trace, error.as_ref())?;
            return Err(error);
        }
    };
    let text = collected_text(&events);
    if text.trim().is_empty() {
        let error: Box<dyn Error> =
            "Gemini Live returned no text delta; expected tracked text output".into();
        emit_failed_probe_report(&credential, &request, &trace, error.as_ref())?;
        return Err(error);
    }
    let trace = trace_snapshot(&trace)?;
    require_trace_stage(&trace, GeminiLiveTraceStage::SetupSent)?;
    require_trace_stage(&trace, GeminiLiveTraceStage::RealtimeAudioSent)?;
    require_trace_stage(&trace, GeminiLiveTraceStage::RealtimeVideoSent)?;
    require_trace_stage(&trace, GeminiLiveTraceStage::TextDeltaReceived)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "status": "ok",
            "provider": "gemini",
            "auth_source": credential.source,
            "model": request.model,
            "audio_bytes": audio.len(),
            "screen_frame_count": screen_frames.len(),
            "screen_frame_bytes": total_len(&screen_frames),
            "text_delta_count": text_delta_count(&events),
            "first_text_delta_ms": first_stage_ms(&trace, GeminiLiveTraceStage::TextDeltaReceived),
            "trace": trace,
            "text": text,
        }))?
    );
    Ok(())
}

fn emit_failed_probe_report(
    credential: &CredentialSelection,
    request: &LlmRequest,
    trace: &Arc<Mutex<Vec<TraceRecord>>>,
    error: &dyn Error,
) -> Result<(), Box<dyn Error>> {
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "status": "failed",
            "provider": "gemini",
            "auth_source": credential.source,
            "model": request.model,
            "error": error.to_string(),
            "trace": trace_snapshot(trace)?,
        }))?
    );
    Ok(())
}

fn live_probe_request(model: &str, audio: Vec<u8>, screen_frames: Vec<Vec<u8>>) -> LlmRequest {
    let mut modality_inputs = vec![audio_input(audio)];
    modality_inputs.extend(
        screen_frames
            .into_iter()
            .enumerate()
            .map(|(index, bytes)| screen_frame_input(index + 1, bytes)),
    );
    LlmRequest {
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: "Reply with exactly: live transport ok".to_string(),
        }],
        stream: true,
        provider_id: Some("gemini".to_string()),
        model: Some(model.to_string()),
        conversation_id: None,
        provider_session_id: None,
        mcp_servers: Vec::new(),
        modality_inputs,
        options: Default::default(),
    }
}

fn provider_config(model: String, credential: String) -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: "gemini".to_string(),
        kind: LlmProviderKind::Gemini,
        model,
        endpoint: env_first(&["LUMVISE_GEMINI_LIVE_ENDPOINT"]),
        credential: Some(credential),
        completion_concurrency: None,
        spawn: Some(SpawnConfig {
            command: "true".to_string(),
            args: Vec::new(),
            timeout_ms: 1_000,
        }),
    }
}

fn traced_provider(
    model: String,
    credential: String,
    trace: Arc<Mutex<Vec<TraceRecord>>>,
) -> Result<GeminiProvider, Box<dyn Error>> {
    let started = Instant::now();
    let trace_sink = Arc::new(move |event: GeminiLiveTraceEvent| {
        let record = TraceRecord {
            elapsed_ms: started.elapsed().as_millis(),
            event,
        };
        trace
            .lock()
            .expect("trace lock should be available")
            .push(record);
    });
    let transport = GeminiLiveWebSocketTransport::new_with_trace(
        env_first(&["LUMVISE_GEMINI_LIVE_ENDPOINT"]),
        trace_sink,
    );
    Ok(GeminiProvider::new_with_live_transport(
        provider_config(model, credential),
        Arc::new(transport),
    )?)
}

fn stream_events(
    provider: &GeminiProvider,
    request: &LlmRequest,
) -> Result<Vec<LlmStreamEvent>, Box<dyn Error>> {
    let mut events = Vec::new();
    provider.stream_with_events(request, StreamControl::unbounded(), &mut |event| {
        events.push(event);
        Ok(())
    })?;
    Ok(events)
}

fn collected_text(events: &[LlmStreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            LlmStreamEvent::ContentDelta { text } => Some(text.as_str()),
            LlmStreamEvent::Session { .. }
            | LlmStreamEvent::FinalText { .. }
            | LlmStreamEvent::Complete
            | LlmStreamEvent::Cancelled
            | LlmStreamEvent::Error { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn text_delta_count(events: &[LlmStreamEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, LlmStreamEvent::ContentDelta { .. }))
        .count()
}

fn total_len(values: &[Vec<u8>]) -> usize {
    values.iter().map(Vec::len).sum()
}

fn trace_snapshot(
    trace: &Arc<Mutex<Vec<TraceRecord>>>,
) -> Result<Vec<TraceRecord>, Box<dyn Error>> {
    trace
        .lock()
        .map(|records| records.clone())
        .map_err(|_| "trace lock should be available".into())
}

fn require_trace_stage(
    trace: &[TraceRecord],
    stage: GeminiLiveTraceStage,
) -> Result<(), Box<dyn Error>> {
    if trace.iter().any(|record| record.event.stage == stage) {
        return Ok(());
    }
    Err(format!("Gemini Live trace did not record required stage `{stage:?}`").into())
}

fn first_stage_ms(trace: &[TraceRecord], stage: GeminiLiveTraceStage) -> Option<u128> {
    trace
        .iter()
        .find(|record| record.event.stage == stage)
        .map(|record| record.elapsed_ms)
}

fn audio_input(bytes: Vec<u8>) -> LlmModalityInput {
    LlmModalityInput {
        input_id: "runtime-probe-audio".to_string(),
        kind: LlmModalityInputKind::LiveAudioChunk,
        media_type: "audio/pcm;rate=16000".to_string(),
        bytes,
        metadata: json!({ "source": "scripted_runtime_probe" }),
    }
}

fn screen_frame_input(index: usize, bytes: Vec<u8>) -> LlmModalityInput {
    LlmModalityInput {
        input_id: format!("runtime-probe-screen-frame-{index}"),
        kind: LlmModalityInputKind::ScreenFrame,
        media_type: "image/png".to_string(),
        bytes,
        metadata: json!({ "source": "scripted_runtime_probe" }),
    }
}

fn synthetic_pcm16_audio(sample_rate: usize, duration_ms: usize) -> Vec<u8> {
    let sample_count = sample_rate * duration_ms / 1_000;
    let mut bytes = Vec::with_capacity(sample_count * 2);
    for index in 0..sample_count {
        let phase = index as f32 * 440.0 * std::f32::consts::TAU / sample_rate as f32;
        let sample = (phase.sin() * i16::MAX as f32 * 0.08) as i16;
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

fn one_pixel_png() -> Vec<u8> {
    vec![
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 15, 4, 0, 9,
        251, 3, 253, 167, 111, 129, 157, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ]
}

fn env_first(keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.trim().is_empty())
}

struct CredentialSelection {
    value: String,
    source: &'static str,
}

fn gemini_live_credential() -> Result<CredentialSelection, Box<dyn Error>> {
    if let Some(value) = env_first(&["LUMVISE_GEMINI_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY"])
    {
        return Ok(CredentialSelection {
            value,
            source: "api_key_env",
        });
    }
    if let Some(value) = env_first(&["LUMVISE_GEMINI_OAUTH_ACCESS_TOKEN"]) {
        return Ok(CredentialSelection {
            value: format!("oauth:{value}"),
            source: "oauth_env",
        });
    }
    if let Some(value) = gemini_cli_oauth_access_token()? {
        return Ok(CredentialSelection {
            value: format!("oauth:{value}"),
            source: "gemini_cli_oauth",
        });
    }
    Err("missing Gemini Live credential; set one of LUMVISE_GEMINI_API_KEY, GEMINI_API_KEY, GOOGLE_API_KEY, LUMVISE_GEMINI_OAUTH_ACCESS_TOKEN, or run `gemini` so ~/.gemini/oauth_creds.json contains a valid access token".into())
}

fn gemini_cli_oauth_access_token() -> Result<Option<String>, Box<dyn Error>> {
    let Some(path) = gemini_cli_oauth_path() else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    if !oauth_token_is_fresh(&value) {
        return Ok(None);
    }
    Ok(value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .map(str::to_string))
}

fn gemini_cli_oauth_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".gemini/oauth_creds.json"))
}

fn oauth_token_is_fresh(value: &Value) -> bool {
    let Some(expiry_date) = value.get("expiry_date").and_then(Value::as_i64) else {
        return false;
    };
    expiry_date > now_ms() + 60_000
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}
