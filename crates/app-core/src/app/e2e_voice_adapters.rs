use lumvise_neural_core::{EngineConfig, SpawnConfig, Text2VoiceService, Voice2TextService};
use std::path::Path;

const E2E_VOICE_TIMEOUT_MS: u64 = 5_000;
pub(crate) const E2E_VOICE_COMMAND_ENV: &str = "LUMVISE_ASSISTANT_E2E_VOICE_COMMAND";

#[derive(Debug, thiserror::Error)]
#[error(
    "invalid `{E2E_VOICE_COMMAND_ENV}` value `{value}`; expected an absolute deterministic voice fixture executable path"
)]
struct E2eVoiceConfigError {
    value: String,
}

pub(crate) fn install_environment_voice_adapters(
    app: crate::AppCore,
) -> Result<crate::AppCore, Box<dyn std::error::Error>> {
    let command = voice_command(std::env::var(E2E_VOICE_COMMAND_ENV).ok())?;
    let (stt, tts) = deterministic_voice_services(&command)?;
    Ok(app
        .with_voice2text_service(stt)
        .with_text2voice_service(tts))
}

fn voice_command(value: Option<String>) -> Result<String, E2eVoiceConfigError> {
    let value = value.ok_or_else(|| invalid_command("<missing>"))?;
    if Path::new(&value).is_absolute() && Path::new(&value).is_file() {
        return Ok(value);
    }
    Err(invalid_command(&value))
}

fn invalid_command(value: &str) -> E2eVoiceConfigError {
    E2eVoiceConfigError {
        value: value.to_string(),
    }
}

pub(crate) fn deterministic_voice_services(
    command: &str,
) -> Result<(Voice2TextService, Text2VoiceService), Box<dyn std::error::Error>> {
    let stt = Voice2TextService::new(spawned_voice_config(command, "stt"))?;
    let tts = Text2VoiceService::new(spawned_voice_config(command, "tts"))?;
    Ok((stt, tts))
}

fn spawned_voice_config(command: &str, mode: &str) -> EngineConfig {
    EngineConfig {
        engine_id: format!("assistant-e2e-{mode}"),
        spawn: SpawnConfig {
            command: command.to_string(),
            args: vec![mode.to_string()],
            timeout_ms: E2E_VOICE_TIMEOUT_MS,
        },
        expected_dimensions: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{E2E_VOICE_COMMAND_ENV, deterministic_voice_services, voice_command};
    use lumvise_neural_core::text2voice::Text2VoiceRequest;
    use lumvise_neural_core::voice2text::Voice2TextRequest;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    struct DeterministicVoiceExecutable {
        directory: tempfile::TempDir,
    }

    #[test]
    fn voice_command_rejects_missing_executable_path() {
        let error = voice_command(None).unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "invalid `{E2E_VOICE_COMMAND_ENV}` value `<missing>`; expected an absolute deterministic voice fixture executable path"
            )
        );
    }

    impl DeterministicVoiceExecutable {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("deterministic-voice");
            fs::write(&path, Self::script()).unwrap();
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).unwrap();
            Self { directory }
        }

        fn path(&self) -> String {
            self.directory
                .path()
                .join("deterministic-voice")
                .display()
                .to_string()
        }

        fn script() -> &'static str {
            r#"#!/usr/bin/env python3
import struct
import sys

def varint(value):
    out = bytearray()
    while value > 127:
        out.append((value & 127) | 128)
        value >>= 7
    out.append(value)
    return bytes(out)

def field_bytes(number, value):
    return varint(number << 3 | 2) + varint(len(value)) + value

def field_varint(number, value):
    return varint(number << 3) + varint(value)

def read_varint(payload, offset):
    value = 0
    shift = 0
    while True:
        byte = payload[offset]
        offset += 1
        value |= (byte & 127) << shift
        if byte < 128:
            return value, offset
        shift += 7

def request_fields(payload):
    request_id = b""
    operation = 0
    offset = 0
    while offset < len(payload):
        key, offset = read_varint(payload, offset)
        number, wire = key >> 3, key & 7
        if wire == 0:
            value, offset = read_varint(payload, offset)
            if number == 3:
                operation = value
        elif wire == 2:
            size, offset = read_varint(payload, offset)
            value = payload[offset:offset + size]
            offset += size
            if number == 4:
                request_id = value
        elif wire == 5:
            offset += 4
        elif wire == 1:
            offset += 8
        else:
            raise ValueError(f"unsupported protobuf wire type {wire}")
    return request_id, operation

length = struct.unpack("<Q", sys.stdin.buffer.read(8))[0]
request_id, operation = request_fields(sys.stdin.buffer.read(length))
response = field_varint(1, 1) + field_varint(2, 3) + field_varint(3, operation)
response += field_bytes(4, request_id)
if sys.argv[1] == "stt":
    response += field_bytes(6, b"deterministic transcript")
    response += field_bytes(7, b"fixture") + field_bytes(14, b"en")
    response += varint(15 << 3 | 5) + struct.pack("<f", 1.0)
    response += field_varint(16, 1) + field_bytes(20, b"assistant-e2e-stt")
elif sys.argv[1] == "tts":
    response += field_bytes(7, b"fixture") + field_bytes(8, b"audio/wav")
    response += field_bytes(9, b"RIFF") + field_varint(19, 24000)
    response += field_bytes(20, b"assistant-e2e-tts")
else:
    raise ValueError(f"unsupported voice fixture mode {sys.argv[1]}")
response += field_bytes(22, b"{}")
sys.stdout.buffer.write(struct.pack("<Q", len(response)) + response)
"#
        }
    }

    #[test]
    fn deterministic_voice_services_run_injected_stt_and_tts_executable() {
        let executable = DeterministicVoiceExecutable::new();
        let (stt, tts) = deterministic_voice_services(&executable.path()).unwrap();

        let transcript = stt
            .transcribe(&Voice2TextRequest {
                audio: vec![1, 2, 3],
                media_type: "audio/pcm".to_string(),
                model: Some("fixture".to_string()),
            })
            .unwrap();
        let speech = tts
            .synthesize(&Text2VoiceRequest {
                text: "hello".to_string(),
                voice_id: None,
                model: Some("fixture".to_string()),
            })
            .unwrap();

        assert_eq!(transcript.transcript, "deterministic transcript");
        assert_eq!(speech.media_type, "audio/wav");
    }
}
