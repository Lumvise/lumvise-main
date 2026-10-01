use lumvise_neural_core::process::{
    SPAWNED_ENGINE_PROTOCOL_MAJOR, SpawnedEnvelope, SpawnedEnvelopeKind, SpawnedOperation,
};
use prost::Message;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

fn main() {
    if let Err(error) = run_fixture() {
        eprintln!("spawned-engine-fixture: {error}");
        std::process::exit(2);
    }
}

fn run_fixture() -> Result<(), String> {
    let mode = argument("--mode").unwrap_or_else(|| "standard".into());
    if mode == "silent-once" && first_silent_invocation()? {
        std::thread::sleep(Duration::from_secs(10));
        return Ok(());
    }
    if mode == "silent" {
        std::thread::sleep(Duration::from_secs(10));
        return Ok(());
    }
    let request = read_request()?;
    if mode == "require-mcp" && request.mcp_servers.is_empty() {
        return write_envelope(failure(&request, "fixture expected MCP server"));
    }
    if mode == "malformed" {
        return write_raw_frame(&[0xff, 0xff, 0xff]);
    }
    if mode == "failure" {
        write_envelope(failure(&request, "fixture provider failure"))?;
        return finish_marker();
    }
    let mut responses = standard_responses(&request)?;
    if mode == "malformed-vector" {
        responses[0].dimensions = 99;
    }
    let response_count = responses.len();
    for (index, response) in responses.into_iter().enumerate() {
        write_envelope(response)?;
        delay_between_stream_frames(index, response_count)?;
    }
    finish_marker()
}

fn read_request() -> Result<SpawnedEnvelope, String> {
    let mut length = [0_u8; 8];
    std::io::stdin()
        .read_exact(&mut length)
        .map_err(|error| error.to_string())?;
    let length = usize::try_from(u64::from_le_bytes(length)).map_err(|error| error.to_string())?;
    let mut payload = vec![0; length];
    std::io::stdin()
        .read_exact(&mut payload)
        .map_err(|error| error.to_string())?;
    SpawnedEnvelope::decode(payload.as_slice()).map_err(|error| error.to_string())
}

fn standard_responses(request: &SpawnedEnvelope) -> Result<Vec<SpawnedEnvelope>, String> {
    let operation = SpawnedOperation::try_from(request.operation)
        .map_err(|_| format!("unsupported fixture operation {}", request.operation))?;
    let responses = match operation {
        SpawnedOperation::LlmComplete => vec![llm_response(request, SpawnedEnvelopeKind::Data)],
        SpawnedOperation::LlmStream => vec![
            llm_response(request, SpawnedEnvelopeKind::Data),
            completed(request),
        ],
        SpawnedOperation::TextVector => vec![vector_response(request)],
        SpawnedOperation::TextToVoice => vec![voice_response(request, 0)],
        SpawnedOperation::TextToVoiceStream => vec![
            voice_response(request, 1),
            voice_response(request, 2),
            completed(request),
        ],
        SpawnedOperation::VoiceToText => vec![transcript_response(request, 0, true)],
        SpawnedOperation::VoiceToTextStream => vec![
            transcript_response(request, 1, false),
            transcript_response(request, 2, true),
            completed(request),
        ],
        SpawnedOperation::Unspecified => return Err("unspecified fixture operation".into()),
    };
    Ok(responses)
}

fn response(request: &SpawnedEnvelope, kind: SpawnedEnvelopeKind) -> SpawnedEnvelope {
    SpawnedEnvelope {
        protocol_major: SPAWNED_ENGINE_PROTOCOL_MAJOR,
        kind: kind as i32,
        operation: request.operation,
        request_id: request.request_id.clone(),
        model: request.model.clone(),
        engine_id: "spawned-engine-fixture-v1".into(),
        ..SpawnedEnvelope::default()
    }
}

fn llm_response(request: &SpawnedEnvelope, kind: SpawnedEnvelopeKind) -> SpawnedEnvelope {
    SpawnedEnvelope {
        text: "fixture response".into(),
        provider_id: "local-fixture".into(),
        ..response(request, kind)
    }
}

fn vector_response(request: &SpawnedEnvelope) -> SpawnedEnvelope {
    let vector = if request.text == "large-vector" {
        vec![0.25; 100_000]
    } else {
        vec![0.4, 0.5]
    };
    SpawnedEnvelope {
        dimensions: vector.len() as u64,
        vector,
        normalized: true,
        ..response(request, SpawnedEnvelopeKind::Data)
    }
}

fn voice_response(request: &SpawnedEnvelope, sequence: u64) -> SpawnedEnvelope {
    SpawnedEnvelope {
        binary: vec![1, 2, 3, u8::try_from(sequence).unwrap_or_default()],
        media_type: "audio/wav".into(),
        sequence,
        sample_rate_hz: 24_000,
        ..response(request, SpawnedEnvelopeKind::Data)
    }
}

fn transcript_response(
    request: &SpawnedEnvelope,
    sequence: u64,
    is_final: bool,
) -> SpawnedEnvelope {
    SpawnedEnvelope {
        text: if is_final {
            "fixture transcript"
        } else {
            "fixture"
        }
        .into(),
        language: "en".into(),
        confidence: 0.92,
        has_confidence: true,
        sequence,
        is_final,
        ..response(request, SpawnedEnvelopeKind::Data)
    }
}

fn completed(request: &SpawnedEnvelope) -> SpawnedEnvelope {
    response(request, SpawnedEnvelopeKind::Complete)
}

fn failure(request: &SpawnedEnvelope, message: &str) -> SpawnedEnvelope {
    SpawnedEnvelope {
        error_message: message.into(),
        ..response(request, SpawnedEnvelopeKind::Failure)
    }
}

fn write_envelope(envelope: SpawnedEnvelope) -> Result<(), String> {
    write_raw_frame(&envelope.encode_to_vec())
}

fn write_raw_frame(payload: &[u8]) -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&(payload.len() as u64).to_le_bytes())
        .map_err(|error| error.to_string())?;
    stdout
        .write_all(payload)
        .map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())
}

fn finish_marker() -> Result<(), String> {
    let Some(path) = argument("--exit-marker").map(PathBuf::from) else {
        return Ok(());
    };
    std::fs::write(path, b"exited").map_err(|error| error.to_string())
}

fn first_silent_invocation() -> Result<bool, String> {
    let path = argument("--state").ok_or("silent-once requires --state")?;
    if std::path::Path::new(&path).exists() {
        return Ok(false);
    }
    std::fs::write(path, b"started").map_err(|error| error.to_string())?;
    Ok(true)
}

fn delay_between_stream_frames(index: usize, response_count: usize) -> Result<(), String> {
    if index + 1 == response_count {
        return Ok(());
    }
    let milliseconds = argument("--stream-delay-ms")
        .map(|value| value.parse::<u64>().map_err(|error| error.to_string()))
        .transpose()?
        .unwrap_or_default();
    std::thread::sleep(Duration::from_millis(milliseconds));
    Ok(())
}

fn argument(name: &str) -> Option<String> {
    let mut args = std::env::args();
    while let Some(argument) = args.next() {
        if argument == name {
            return args.next();
        }
    }
    None
}
