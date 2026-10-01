use crate::error::{NeuralError, Result, require_non_empty};
use crate::types::EngineMetadata;
use crate::voice2text::model_assets::{path_string, resolve_whisper_model_path};
use crate::voice2text::{Voice2TextRequest, Voice2TextResponse, Voice2TextStreamEvent};
use serde_json::json;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const WHISPER_SAMPLE_RATE_HZ: u32 = 16_000;
const DEFAULT_WHISPER_MODEL_ALIAS: &str = "large-v3-turbo";
const DEFAULT_INITIAL_PROMPT: &str = "Transcribe the speaker's words as literally as possible. Keep wording, names, commands, and short hesitations when they are audible.";

#[derive(Clone)]
pub struct WhisperRsVoice2TextConfig {
    pub engine_id: String,
    pub model_path: String,
    pub language: Option<String>,
    pub threads: i32,
    pub beam_size: i32,
    pub decode: WhisperRsDecodeSettings,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WhisperRsDecodeSettings {
    pub initial_prompt: Option<String>,
    pub temperature: f32,
    pub translate: bool,
    pub no_timestamps: bool,
    pub detect_language: bool,
    pub suppress_blank: bool,
    pub no_speech_threshold: f32,
    pub audio_ctx: i32,
    pub max_len: i32,
}

pub struct WhisperRsVoice2TextEngine {
    config: WhisperRsVoice2TextConfig,
    context: WhisperContext,
}

impl WhisperRsVoice2TextConfig {
    /// Builds config that downloads the default whisper.cpp English model on first use.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = WhisperRsVoice2TextConfig::default_english("whisper");
    /// ```
    pub fn default_english(engine_id: &str) -> Self {
        Self::english_model(engine_id, DEFAULT_WHISPER_MODEL_ALIAS)
    }

    /// Builds config that downloads the whisper.cpp base English model on first use.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = WhisperRsVoice2TextConfig::base_english("whisper");
    /// ```
    pub fn base_english(engine_id: &str) -> Self {
        Self::english_model(engine_id, "base.en")
    }

    /// Builds config that downloads the whisper.cpp large-v3-turbo model on first use.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = WhisperRsVoice2TextConfig::large_v3_turbo("whisper");
    /// ```
    pub fn large_v3_turbo(engine_id: &str) -> Self {
        Self::english_model(engine_id, "large-v3-turbo")
    }

    fn english_model(engine_id: &str, model_path: &str) -> Self {
        Self {
            engine_id: engine_id.to_string(),
            model_path: model_path.to_string(),
            language: Some("en".to_string()),
            threads: 4,
            beam_size: 7,
            decode: WhisperRsDecodeSettings::default_quality(),
        }
    }
}

impl WhisperRsDecodeSettings {
    /// Builds Lumvise's default high-accuracy decode settings.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let settings = WhisperRsDecodeSettings::default_quality();
    /// ```
    pub fn default_quality() -> Self {
        Self {
            initial_prompt: Some(DEFAULT_INITIAL_PROMPT.to_string()),
            temperature: 0.2,
            translate: false,
            no_timestamps: true,
            detect_language: false,
            suppress_blank: true,
            no_speech_threshold: 0.7,
            audio_ctx: 0,
            max_len: 0,
        }
    }
}

impl WhisperRsVoice2TextEngine {
    /// Creates a whisper-rs engine from a local whisper.cpp model file.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use lumvise_neural_core::voice2text::WhisperRsVoice2TextConfig;
    /// let config = WhisperRsVoice2TextConfig {
    ///     engine_id: "whisper".into(),
    ///     model_path: "models/ggml-base.en.bin".into(),
    ///     language: Some("en".into()),
    ///     threads: 4,
    ///     beam_size: 7,
    ///     decode: WhisperRsDecodeSettings::default_quality(),
    /// };
    /// ```
    pub fn new(config: WhisperRsVoice2TextConfig) -> Result<Self> {
        validate_config(&config)?;
        whisper_rs::install_logging_hooks();
        let params = WhisperContextParameters::default();
        let model_path = resolve_whisper_model_path(&config.model_path)?;
        let context = WhisperContext::new_with_params(path_string(&model_path), params)
            .map_err(|error| whisper_error(&config.engine_id, error))?;
        Ok(Self { config, context })
    }

    pub fn transcribe(&self, request: &Voice2TextRequest) -> Result<Voice2TextResponse> {
        validate_request(request)?;
        require_requested_model(request.model.as_deref(), &self.config.model_path)?;
        let decoded_audio = decode_wav_audio(&request.audio)?;
        let samples = decoded_audio.into_whisper_samples()?;
        let mut state = self.context.create_state().map_err(self.error_mapper())?;
        state
            .full(self.full_params(), &samples)
            .map_err(self.error_mapper())?;
        let segments = collect_segments(&state)?;
        Ok(self.response(segments))
    }

    pub fn stream(&self, request: &Voice2TextRequest) -> Result<Vec<Voice2TextStreamEvent>> {
        let response = self.transcribe(request)?;
        let mut events = transcript_events(&response.segments);
        events.push(Voice2TextStreamEvent::Complete);
        Ok(events)
    }

    fn full_params(&self) -> FullParams<'_, '_> {
        let mut params = FullParams::new(SamplingStrategy::BeamSearch {
            beam_size: self.config.beam_size,
            patience: -1.0,
        });
        params.set_n_threads(self.config.threads);
        params.set_language(self.config.language.as_deref());
        self.apply_decode_settings(&mut params);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params
    }

    fn apply_decode_settings(&self, params: &mut FullParams<'_, '_>) {
        let decode = &self.config.decode;
        if let Some(prompt) = decode.initial_prompt.as_deref() {
            params.set_initial_prompt(prompt);
        }
        params.set_temperature(decode.temperature);
        params.set_translate(decode.translate);
        params.set_no_timestamps(decode.no_timestamps);
        params.set_detect_language(decode.detect_language);
        params.set_suppress_blank(decode.suppress_blank);
        params.set_no_speech_thold(decode.no_speech_threshold);
        params.set_audio_ctx(decode.audio_ctx);
        params.set_max_len(decode.max_len);
    }

    fn response(&self, segments: Vec<WhisperSegment>) -> Voice2TextResponse {
        Voice2TextResponse {
            transcript: joined_transcript(&segments),
            language: self.config.language.clone(),
            confidence: None,
            segments: segments_to_json(&segments),
            metadata: EngineMetadata {
                engine_id: self.config.engine_id.clone(),
                model: Some(self.config.model_path.clone()),
                metadata: json!({ "backend": "whisper-rs" }),
            },
        }
    }

    fn error_mapper(&self) -> impl Fn(whisper_rs::WhisperError) -> NeuralError + '_ {
        |error| whisper_error(&self.config.engine_id, error)
    }
}

fn require_requested_model(requested: Option<&str>, configured: &str) -> Result<()> {
    let Some(requested) = requested.filter(|value| !value.trim().is_empty()) else {
        return Ok(());
    };
    if requested == configured {
        return Ok(());
    }
    Err(NeuralError::InvalidValue {
        value: requested.to_string(),
        expected: format!("loaded whisper-rs model {configured}"),
    })
}

#[derive(Debug, Clone, PartialEq)]
struct DecodedWavAudio {
    samples: Vec<f32>,
    channels: u16,
    sample_rate_hz: u32,
}

impl DecodedWavAudio {
    fn into_whisper_samples(self) -> Result<Vec<f32>> {
        if self.sample_rate_hz != WHISPER_SAMPLE_RATE_HZ {
            return Err(NeuralError::InvalidValue {
                value: self.sample_rate_hz.to_string(),
                expected: "16kHz WAV sample rate for whisper-rs".to_string(),
            });
        }
        match self.channels {
            1 => Ok(self.samples),
            2 => Ok(stereo_to_mono(&self.samples)),
            channels => Err(NeuralError::InvalidValue {
                value: channels.to_string(),
                expected: "mono or stereo WAV audio".to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct WhisperSegment {
    sequence: u64,
    start_ms: i64,
    end_ms: i64,
    text: String,
}

fn validate_config(config: &WhisperRsVoice2TextConfig) -> Result<()> {
    require_non_empty(&config.engine_id, "non-empty whisper-rs engine id")?;
    require_non_empty(&config.model_path, "whisper-rs model path")?;
    if config.threads < 1 {
        return invalid_i32(config.threads, "at least one whisper-rs thread");
    }
    if config.beam_size < 1 {
        return invalid_i32(config.beam_size, "positive whisper-rs beam size");
    }
    validate_decode_settings(&config.decode)?;
    Ok(())
}

fn validate_decode_settings(decode: &WhisperRsDecodeSettings) -> Result<()> {
    if !(0.0..=1.0).contains(&decode.temperature) {
        return invalid_f32(
            decode.temperature,
            "whisper-rs temperature between 0.0 and 1.0",
        );
    }
    if !(0.0..=1.0).contains(&decode.no_speech_threshold) {
        return invalid_f32(
            decode.no_speech_threshold,
            "whisper-rs no-speech threshold between 0.0 and 1.0",
        );
    }
    Ok(())
}

fn validate_request(request: &Voice2TextRequest) -> Result<()> {
    if request.audio.is_empty() {
        return Err(NeuralError::InvalidValue {
            value: "empty audio".to_string(),
            expected: "non-empty WAV audio bytes".to_string(),
        });
    }
    if !request.media_type.contains("wav") {
        return Err(NeuralError::InvalidValue {
            value: request.media_type.clone(),
            expected: "WAV media type for whisper-rs".to_string(),
        });
    }
    Ok(())
}

fn decode_wav_audio(bytes: &[u8]) -> Result<DecodedWavAudio> {
    ensure_riff_wave(bytes)?;
    let fmt = find_chunk(bytes, b"fmt ")?;
    let data = find_chunk(bytes, b"data")?;
    let format = parse_wav_format(fmt)?;
    let samples = decode_samples(data, format.audio_format, format.bits_per_sample)?;
    Ok(DecodedWavAudio {
        samples,
        channels: format.channels,
        sample_rate_hz: format.sample_rate_hz,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WavFormat {
    audio_format: u16,
    channels: u16,
    sample_rate_hz: u32,
    bits_per_sample: u16,
}

fn ensure_riff_wave(bytes: &[u8]) -> Result<()> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(NeuralError::MalformedPayload {
            value: "audio bytes".to_string(),
            expected: "RIFF/WAVE header".to_string(),
        });
    }
    Ok(())
}

fn find_chunk<'a>(bytes: &'a [u8], chunk_id: &[u8; 4]) -> Result<&'a [u8]> {
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let start = offset + 8;
        let end = start + size;
        if end > bytes.len() {
            break;
        }
        if &bytes[offset..offset + 4] == chunk_id {
            return Ok(&bytes[start..end]);
        }
        offset = start + size + size % 2;
    }
    Err(NeuralError::MalformedPayload {
        value: String::from_utf8_lossy(chunk_id).to_string(),
        expected: "present WAV chunk".to_string(),
    })
}

fn parse_wav_format(bytes: &[u8]) -> Result<WavFormat> {
    if bytes.len() < 16 {
        return Err(NeuralError::MalformedPayload {
            value: "fmt chunk".to_string(),
            expected: "16-byte PCM WAV format chunk".to_string(),
        });
    }
    Ok(WavFormat {
        audio_format: read_u16(bytes, 0),
        channels: read_u16(bytes, 2),
        sample_rate_hz: read_u32(bytes, 4),
        bits_per_sample: read_u16(bytes, 14),
    })
}

fn decode_samples(bytes: &[u8], format: u16, bits: u16) -> Result<Vec<f32>> {
    ensure_sample_alignment(bytes, bits)?;
    match (format, bits) {
        (1, 16) => Ok(bytes.chunks_exact(2).map(sample_i16).collect()),
        (3, 32) => Ok(bytes.chunks_exact(4).map(sample_f32).collect()),
        _ => Err(NeuralError::InvalidValue {
            value: format!("{format}/{bits}"),
            expected: "16-bit PCM or 32-bit float WAV samples".to_string(),
        }),
    }
}

fn ensure_sample_alignment(bytes: &[u8], bits: u16) -> Result<()> {
    let bytes_per_sample = usize::from(bits / 8);
    if bytes_per_sample != 0 && bytes.len().is_multiple_of(bytes_per_sample) {
        return Ok(());
    }
    Err(NeuralError::MalformedPayload {
        value: bytes.len().to_string(),
        expected: "WAV data length aligned to sample width".to_string(),
    })
}

fn collect_segments(state: &whisper_rs::WhisperState) -> Result<Vec<WhisperSegment>> {
    state
        .as_iter()
        .enumerate()
        .map(|(sequence, segment)| {
            let text = segment.to_str_lossy().map_err(NeuralError::from)?;
            Ok(WhisperSegment {
                sequence: sequence as u64,
                start_ms: segment.start_timestamp() * 10,
                end_ms: segment.end_timestamp() * 10,
                text: text.trim().to_string(),
            })
        })
        .collect()
}

fn transcript_events(segments: &[serde_json::Value]) -> Vec<Voice2TextStreamEvent> {
    segments
        .iter()
        .filter_map(segment_event)
        .collect::<Vec<Voice2TextStreamEvent>>()
}

fn segment_event(segment: &serde_json::Value) -> Option<Voice2TextStreamEvent> {
    Some(Voice2TextStreamEvent::TranscriptChunk {
        sequence: segment.get("sequence")?.as_u64()?,
        text: segment.get("text")?.as_str()?.to_string(),
        is_final: true,
    })
}

fn segments_to_json(segments: &[WhisperSegment]) -> Vec<serde_json::Value> {
    segments
        .iter()
        .map(|segment| {
            json!({
                "sequence": segment.sequence,
                "start_ms": segment.start_ms,
                "end_ms": segment.end_ms,
                "text": segment.text
            })
        })
        .collect()
}

fn joined_transcript(segments: &[WhisperSegment]) -> String {
    segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn stereo_to_mono(samples: &[f32]) -> Vec<f32> {
    samples
        .chunks_exact(2)
        .map(|chunk| (chunk[0] + chunk[1]) / 2.0)
        .collect()
}

fn sample_i16(bytes: &[u8]) -> f32 {
    i16::from_le_bytes(bytes.try_into().unwrap()) as f32 / i16::MAX as f32
}

fn sample_f32(bytes: &[u8]) -> f32 {
    f32::from_le_bytes(bytes.try_into().unwrap())
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn invalid_i32(value: i32, expected: &str) -> Result<()> {
    Err(NeuralError::InvalidValue {
        value: value.to_string(),
        expected: expected.to_string(),
    })
}

fn invalid_f32(value: f32, expected: &str) -> Result<()> {
    Err(NeuralError::InvalidValue {
        value: value.to_string(),
        expected: expected.to_string(),
    })
}

fn whisper_error(engine_id: &str, error: whisper_rs::WhisperError) -> NeuralError {
    NeuralError::ProviderFailed {
        provider_id: engine_id.to_string(),
        message: error.to_string(),
    }
}

impl From<whisper_rs::WhisperError> for NeuralError {
    fn from(error: whisper_rs::WhisperError) -> Self {
        whisper_error("whisper-rs", error)
    }
}

#[cfg(test)]
#[path = "whisper_rs_engine_tests.rs"]
mod whisper_rs_engine_tests;
