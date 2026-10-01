#![cfg(any(feature = "fastembed", feature = "kokoros", feature = "whisper-rs"))]

#[cfg(feature = "fastembed")]
use lumvise_neural_core::text2vector::{
    FastEmbedText2VectorConfig, FastEmbedText2VectorEngine, Text2VectorRequest,
};
#[cfg(feature = "kokoros")]
use lumvise_neural_core::text2voice::{
    KokorosText2VoiceConfig, KokorosText2VoiceEngine, Text2VoiceRequest,
};
#[cfg(feature = "whisper-rs")]
use lumvise_neural_core::voice2text::{
    Voice2TextRequest, WhisperRsVoice2TextConfig, WhisperRsVoice2TextEngine,
};
use std::path::PathBuf;

const WHISPER_MODEL: &str = "base.en";
const KOKOROS_MODEL: &str = "kokoro-v1.0";
const KOKOROS_VOICE: &str = "af_sky";
const FASTEMBED_MODEL: &str = "bge-small-en-v1.5";

#[cfg(feature = "whisper-rs")]
#[test]
#[ignore = "explicit cache-aware native model canary; run scripts/canaries/native-models"]
fn native_whisper_transcribes_fixed_speech_fixture() {
    report_model("whisper-rs", WHISPER_MODEL);
    let engine = WhisperRsVoice2TextEngine::new(whisper_config()).unwrap();
    let response = engine
        .transcribe(&Voice2TextRequest {
            audio: std::fs::read(speech_fixture()).unwrap(),
            media_type: "audio/wav".into(),
            model: None,
        })
        .unwrap();

    assert_transcript_contains(
        &response.transcript,
        "ask not what your country can do for you",
    );
    assert!(!response.segments.is_empty());
}

#[cfg(feature = "kokoros")]
#[test]
#[ignore = "explicit cache-aware native model canary; run scripts/canaries/native-models"]
fn native_kokoros_produces_non_silent_wav() {
    report_model("kokoros", KOKOROS_MODEL);
    let engine = KokorosText2VoiceEngine::new(kokoros_config()).unwrap();
    let response = engine
        .synthesize(&tts_request("Lumvise native voice canary."))
        .unwrap();

    assert_eq!(response.media_type, "audio/wav");
    assert_eq!(response.sample_rate_hz, Some(24_000));
    assert_eq!(&response.audio[..4], b"RIFF");
    assert_eq!(&response.audio[8..12], b"WAVE");
    assert!(pcm_peak(&response.audio[44..]) > 100);
}

#[cfg(all(feature = "kokoros", feature = "whisper-rs"))]
#[test]
#[ignore = "explicit cache-aware native model canary; run scripts/canaries/native-models"]
fn native_kokoros_audio_round_trips_through_whisper() {
    report_model("kokoros-to-whisper", "kokoro-v1.0 + base.en");
    let tts = KokorosText2VoiceEngine::new(kokoros_config()).unwrap();
    let whisper = WhisperRsVoice2TextEngine::new(whisper_config()).unwrap();
    let generated = tts.synthesize(&tts_request("Hello neural core.")).unwrap();
    let response = whisper
        .transcribe(&Voice2TextRequest {
            audio: resample_kokoros_wav(&generated.audio),
            media_type: "audio/wav".into(),
            model: None,
        })
        .unwrap();

    assert_transcript_contains(&response.transcript, "neural core");
}

#[cfg(feature = "fastembed")]
#[test]
#[ignore = "explicit cache-aware native model canary; run scripts/canaries/native-models"]
fn native_fastembed_distinguishes_unrelated_inputs() {
    report_model("fastembed", FASTEMBED_MODEL);
    let mut config = FastEmbedText2VectorConfig::builtin("native-canary", FASTEMBED_MODEL);
    config.cache_dir = Some(model_cache().join("fastembed"));
    let engine = FastEmbedText2VectorEngine::new(config).unwrap();
    let anchor = embed(&engine, "semantic memory routing for source code");
    let related = embed(&engine, "route project code into semantic memory");
    let unrelated = embed(&engine, "invoice payment reconciliation");

    assert_eq!(anchor.len(), 384);
    assert!(anchor.iter().any(|value| value.abs() > 0.0001));
    assert!(cosine_similarity(&anchor, &related) > cosine_similarity(&anchor, &unrelated));
}

#[cfg(feature = "fastembed")]
fn embed(engine: &FastEmbedText2VectorEngine, text: &str) -> Vec<f32> {
    let response = engine
        .embed(&Text2VectorRequest {
            text: text.into(),
            model: None,
        })
        .unwrap();
    assert_eq!(response.dimensions, response.vector.len());
    assert_eq!(response.metadata.model.as_deref(), Some(FASTEMBED_MODEL));
    response.vector
}

#[cfg(feature = "fastembed")]
fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    let dot = left.iter().zip(right).map(|(a, b)| a * b).sum::<f32>();
    let left_norm = left.iter().map(|value| value * value).sum::<f32>().sqrt();
    let right_norm = right.iter().map(|value| value * value).sum::<f32>().sqrt();
    dot / (left_norm * right_norm)
}

#[cfg(feature = "whisper-rs")]
fn whisper_config() -> WhisperRsVoice2TextConfig {
    let mut config = WhisperRsVoice2TextConfig::base_english("native-canary-whisper");
    if let Ok(path) = std::env::var("LUMVISE_NATIVE_CANARY_WHISPER_MODEL") {
        config.model_path = path;
    }
    config
}

#[cfg(feature = "kokoros")]
fn kokoros_config() -> KokorosText2VoiceConfig {
    let mut config = KokorosText2VoiceConfig::kokoro_v1("native-canary-kokoros", KOKOROS_VOICE);
    if let Ok(path) = std::env::var("LUMVISE_NATIVE_CANARY_KOKOROS_MODEL") {
        config.model_path = path;
    }
    config
}

#[cfg(feature = "kokoros")]
fn tts_request(text: &str) -> Text2VoiceRequest {
    Text2VoiceRequest {
        text: text.into(),
        voice_id: None,
        model: None,
    }
}

fn model_cache() -> PathBuf {
    std::env::var_os("LUMVISE_NEURAL_MODEL_CACHE")
        .map(PathBuf::from)
        .expect("missing LUMVISE_NEURAL_MODEL_CACHE; expected scripts/canaries/native-models")
}

fn report_model(engine: &str, model: &str) {
    eprintln!(
        "native-model-canary engine={engine} model={model} cache={}",
        model_cache().display()
    );
}

#[cfg(feature = "whisper-rs")]
fn speech_fixture() -> PathBuf {
    std::env::var_os("LUMVISE_NATIVE_CANARY_SPEECH_FIXTURE")
        .map(PathBuf::from)
        .expect("missing speech fixture; expected scripts/canaries/native-models to cache jfk.wav")
}

#[cfg(feature = "kokoros")]
fn pcm_peak(bytes: &[u8]) -> i16 {
    bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]).saturating_abs())
        .max()
        .unwrap_or(0)
}

#[cfg(all(feature = "kokoros", feature = "whisper-rs"))]
fn resample_kokoros_wav(bytes: &[u8]) -> Vec<u8> {
    assert_eq!(
        u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
        24_000
    );
    let samples = decode_pcm16(&bytes[44..]);
    let output_length = samples.len() * 2 / 3;
    let resampled = (0..output_length)
        .map(|index| interpolate(&samples, index as f32 * 1.5))
        .collect::<Vec<_>>();
    encode_pcm16_wav(&resampled, 16_000)
}

#[cfg(all(feature = "kokoros", feature = "whisper-rs"))]
fn decode_pcm16(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / i16::MAX as f32)
        .collect()
}

#[cfg(all(feature = "kokoros", feature = "whisper-rs"))]
fn interpolate(samples: &[f32], position: f32) -> f32 {
    let left = position.floor() as usize;
    let right = (left + 1).min(samples.len().saturating_sub(1));
    let weight = position - left as f32;
    samples[left] * (1.0 - weight) + samples[right] * weight
}

#[cfg(all(feature = "kokoros", feature = "whisper-rs"))]
fn encode_pcm16_wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_size = samples.len() as u32 * 2;
    let mut bytes = wav_header(data_size, sample_rate);
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[cfg(all(feature = "kokoros", feature = "whisper-rs"))]
fn wav_header(data_size: u32, sample_rate: u32) -> Vec<u8> {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    bytes.extend_from_slice(b"\x02\0\x10\0data");
    bytes.extend_from_slice(&data_size.to_le_bytes());
    bytes
}

#[cfg(feature = "whisper-rs")]
fn assert_transcript_contains(transcript: &str, expected: &str) {
    let transcript = searchable_text(transcript);
    let expected = searchable_text(expected);
    assert!(
        transcript.contains(&expected),
        "transcript `{transcript}` did not contain `{expected}`"
    );
}

#[cfg(feature = "whisper-rs")]
fn searchable_text(value: &str) -> String {
    value
        .chars()
        .filter(|char| char.is_ascii_alphanumeric())
        .flat_map(|char| char.to_lowercase())
        .collect()
}
