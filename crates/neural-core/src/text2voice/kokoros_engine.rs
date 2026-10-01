use super::kokoros_assets::{KokorosSherpaAssets, default_model_alias, resolve_kokoros_assets};
use crate::error::{NeuralError, Result, require_non_empty};
use crate::text2voice::speech_text::prepare_text2voice_request;
use crate::text2voice::{Text2VoiceRequest, Text2VoiceResponse, Text2VoiceStreamEvent};
use crate::types::EngineMetadata;
use serde_json::json;
use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig,
};
use std::path::Path;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};

const KOKOROS_SAMPLE_RATE_HZ: u32 = 24_000;

#[derive(Clone)]
pub struct KokorosText2VoiceConfig {
    pub engine_id: String,
    pub model_path: String,
    pub voices_path: String,
    pub default_voice_id: String,
    pub speed: f32,
}

pub struct KokorosText2VoiceEngine {
    config: KokorosText2VoiceConfig,
    synthesizer: OfflineTts,
}

impl KokorosText2VoiceConfig {
    /// Builds config that downloads Sherpa-ONNX Kokoro assets on first use.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = KokorosText2VoiceConfig::kokoro_v1("kokoros", "af_heart");
    /// ```
    pub fn kokoro_v1(engine_id: &str, default_voice_id: &str) -> Self {
        Self {
            engine_id: engine_id.to_string(),
            model_path: default_model_alias().to_string(),
            voices_path: "voices.bin".to_string(),
            default_voice_id: default_voice_id.to_string(),
            speed: 1.0,
        }
    }
}

impl KokorosText2VoiceEngine {
    /// Creates a Sherpa-ONNX Kokoro text-to-voice engine.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use lumvise_neural_core::text2voice::KokorosText2VoiceConfig;
    /// let config = KokorosText2VoiceConfig::kokoro_v1("kokoros", "af_heart");
    /// ```
    pub fn new(config: KokorosText2VoiceConfig) -> Result<Self> {
        validate_config(&config)?;
        let assets = resolve_kokoros_assets(&config.model_path, &config.voices_path)?;
        let synthesizer = create_sherpa_tts(&config.engine_id, &assets)?;
        Ok(Self {
            config,
            synthesizer,
        })
    }

    pub fn synthesize(&self, request: &Text2VoiceRequest) -> Result<Text2VoiceResponse> {
        let prepared = self.prepare_request(request)?;
        let voice_id = self.resolve_voice_id(&prepared)?;
        let (samples, metadata) = self.generate_samples(&prepared, voice_id)?;
        Ok(Text2VoiceResponse {
            audio: encode_pcm16_wav(&samples),
            media_type: "audio/wav".to_string(),
            sample_rate_hz: Some(KOKOROS_SAMPLE_RATE_HZ),
            metadata: self.metadata(voice_id, metadata),
        })
    }

    pub fn stream(&self, request: &Text2VoiceRequest) -> Result<Vec<Text2VoiceStreamEvent>> {
        let mut events = Vec::new();
        self.stream_with_events(request, |event| {
            events.push(event);
            Ok(())
        })?;
        Ok(events)
    }

    /// Streams generated audio to a caller-owned sink during inference.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let request = Text2VoiceRequest { text: "hello".into(), voice_id: None, model: None };
    /// engine.stream_with_events(&request, |event| {
    ///     handle_text2voice_event(event);
    ///     Ok(())
    /// })?;
    /// ```
    pub fn stream_with_events<F>(&self, request: &Text2VoiceRequest, mut on_event: F) -> Result<()>
    where
        F: FnMut(Text2VoiceStreamEvent) -> Result<()>,
    {
        let prepared = self.prepare_request(request)?;
        let voice_id = self.resolve_voice_id(&prepared)?;
        self.generate_streamed_samples(&prepared, voice_id, &mut on_event)?;
        on_event(Text2VoiceStreamEvent::Complete)
    }

    fn generate_streamed_samples<F>(
        &self,
        request: &Text2VoiceRequest,
        voice_id: &str,
        on_event: &mut F,
    ) -> Result<()>
    where
        F: FnMut(Text2VoiceStreamEvent) -> Result<()>,
    {
        let generation = self.generation_config(voice_id)?;
        let (sender, receiver) = sync_channel(4);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| self.generate_pcm_events(request, &generation, sender));
            let sink_result = receiver.into_iter().try_for_each(on_event);
            let generation_result = join_generation(worker, &self.config.engine_id);
            sink_result?;
            generation_result
        })
    }

    fn generate_pcm_events(
        &self,
        request: &Text2VoiceRequest,
        generation: &GenerationConfig,
        sender: SyncSender<Text2VoiceStreamEvent>,
    ) -> Result<()> {
        let emitter = Arc::new(Mutex::new(PcmStreamEmitter::new()));
        let callback_emitter = Arc::clone(&emitter);
        let callback_sender = sender.clone();
        let audio = self.synthesizer.generate_with_config(
            &request.text,
            generation,
            Some(move |samples: &[f32], _progress: f32| {
                send_pcm_callback(&callback_emitter, &callback_sender, samples)
            }),
        );
        let audio = audio.ok_or_else(|| {
            sherpa_error(
                &self.config.engine_id,
                "stream generation returned no audio",
            )
        })?;
        require_kokoro_sample_rate(&self.config.engine_id, audio.sample_rate())?;
        send_final_pcm_tail(&emitter, &sender, audio.samples(), &self.config.engine_id)
    }

    fn prepare_request(&self, request: &Text2VoiceRequest) -> Result<Text2VoiceRequest> {
        let prepared = prepare_text2voice_request(request);
        require_non_empty(&prepared.text, "non-empty text to synthesize")?;
        require_requested_model(prepared.model.as_deref(), &self.config.model_path)?;
        Ok(prepared)
    }

    fn generate_samples(
        &self,
        request: &Text2VoiceRequest,
        voice_id: &str,
    ) -> Result<(Vec<f32>, serde_json::Value)> {
        let generation = self.generation_config(voice_id)?;
        let audio = self
            .synthesizer
            .generate_with_config(
                &request.text,
                &generation,
                Option::<fn(&[f32], f32) -> bool>::None,
            )
            .ok_or_else(|| sherpa_error(&self.config.engine_id, "generation returned no audio"))?;
        let samples = samples_from_generated_audio(&self.config.engine_id, &audio)?;
        let sid = generation.sid;
        Ok((
            samples,
            json!({ "sid": sid, "sample_rate_hz": audio.sample_rate() }),
        ))
    }

    fn generation_config(&self, voice_id: &str) -> Result<GenerationConfig> {
        Ok(GenerationConfig {
            sid: voice_id_to_sid(voice_id)?,
            speed: self.config.speed,
            ..Default::default()
        })
    }

    fn resolve_voice_id<'a>(&'a self, request: &'a Text2VoiceRequest) -> Result<&'a str> {
        let voice_id = request
            .voice_id
            .as_deref()
            .unwrap_or(&self.config.default_voice_id);
        require_non_empty(voice_id, "non-empty Kokoro voice id")?;
        Ok(voice_id)
    }

    fn metadata(&self, voice_id: &str, metadata: serde_json::Value) -> EngineMetadata {
        EngineMetadata {
            engine_id: self.config.engine_id.clone(),
            model: Some(self.config.model_path.clone()),
            metadata: json!({ "voice_id": voice_id, "kokoro": metadata }),
        }
    }
}

fn create_sherpa_tts(engine_id: &str, assets: &KokorosSherpaAssets) -> Result<OfflineTts> {
    let config = OfflineTtsConfig {
        model: OfflineTtsModelConfig {
            kokoro: kokoro_model_config(assets),
            num_threads: 2,
            debug: false,
            ..Default::default()
        },
        ..Default::default()
    };
    OfflineTts::create(&config).ok_or_else(|| sherpa_error(engine_id, "OfflineTts::create failed"))
}

fn kokoro_model_config(assets: &KokorosSherpaAssets) -> OfflineTtsKokoroModelConfig {
    OfflineTtsKokoroModelConfig {
        model: Some(path_string(&assets.model_path)),
        voices: Some(path_string(&assets.voices_path)),
        tokens: Some(path_string(&assets.tokens_path)),
        data_dir: Some(path_string(&assets.data_dir)),
        length_scale: 1.0,
        ..Default::default()
    }
}

fn samples_from_generated_audio(
    engine_id: &str,
    audio: &sherpa_onnx::GeneratedAudio,
) -> Result<Vec<f32>> {
    require_kokoro_sample_rate(engine_id, audio.sample_rate())?;
    Ok(audio.samples().to_vec())
}

fn require_kokoro_sample_rate(engine_id: &str, actual: i32) -> Result<()> {
    let sample_rate = u32::try_from(actual).unwrap_or(0);
    if sample_rate != KOKOROS_SAMPLE_RATE_HZ {
        return Err(sherpa_error(
            engine_id,
            &format!("sample rate {sample_rate}; expected {KOKOROS_SAMPLE_RATE_HZ}"),
        ));
    }
    Ok(())
}

struct PcmStreamEmitter {
    callback_samples: usize,
    sequence: u64,
}

impl PcmStreamEmitter {
    fn new() -> Self {
        Self {
            callback_samples: 0,
            sequence: 0,
        }
    }

    fn callback_event(&mut self, samples: &[f32]) -> Option<Text2VoiceStreamEvent> {
        if samples.is_empty() {
            return None;
        }
        let event = pcm_audio_chunk(self.sequence, samples);
        self.callback_samples += samples.len();
        self.sequence += 1;
        Some(event)
    }

    fn final_tail_event(&mut self, samples: &[f32]) -> Option<Text2VoiceStreamEvent> {
        let tail = samples.get(self.callback_samples..)?;
        if tail.is_empty() {
            return None;
        }
        let event = pcm_audio_chunk(self.sequence, tail);
        self.callback_samples = samples.len();
        self.sequence += 1;
        Some(event)
    }
}

fn send_pcm_callback(
    emitter: &Mutex<PcmStreamEmitter>,
    sender: &SyncSender<Text2VoiceStreamEvent>,
    samples: &[f32],
) -> bool {
    let Ok(mut emitter) = emitter.lock() else {
        return false;
    };
    emitter
        .callback_event(samples)
        .is_none_or(|event| sender.send(event).is_ok())
}

fn send_final_pcm_tail(
    emitter: &Mutex<PcmStreamEmitter>,
    sender: &SyncSender<Text2VoiceStreamEvent>,
    samples: &[f32],
    engine_id: &str,
) -> Result<()> {
    let event = emitter
        .lock()
        .map_err(|_| sherpa_error(engine_id, "PCM stream emitter lock was poisoned"))?
        .final_tail_event(samples);
    event.map_or(Ok(()), |event| {
        sender
            .send(event)
            .map_err(|_| sherpa_error(engine_id, "PCM stream receiver closed before completion"))
    })
}

fn join_generation(
    worker: std::thread::ScopedJoinHandle<'_, Result<()>>,
    engine_id: &str,
) -> Result<()> {
    worker
        .join()
        .map_err(|_| sherpa_error(engine_id, "stream generation thread panicked"))?
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
        expected: format!("loaded Kokoro model {configured}"),
    })
}

fn validate_config(config: &KokorosText2VoiceConfig) -> Result<()> {
    require_non_empty(&config.engine_id, "non-empty Kokoro engine id")?;
    require_non_empty(
        &config.model_path,
        "Sherpa Kokoro model path or bundle alias",
    )?;
    require_non_empty(&config.voices_path, "Sherpa Kokoro voices path")?;
    require_non_empty(&config.default_voice_id, "default Kokoro voice id")?;
    if config.speed <= 0.0 {
        return Err(NeuralError::InvalidValue {
            value: config.speed.to_string(),
            expected: "Kokoro speed greater than zero".to_string(),
        });
    }
    Ok(())
}

fn voice_id_to_sid(voice_id: &str) -> Result<i32> {
    match voice_id.trim() {
        "af_alloy" => Ok(0),
        "af_aoede" => Ok(1),
        "af_bella" => Ok(2),
        "af_heart" => Ok(3),
        "af_jessica" => Ok(4),
        "af_kore" => Ok(5),
        "af_nicole" => Ok(6),
        "af_nova" => Ok(7),
        "af_river" => Ok(8),
        "af_sarah" => Ok(9),
        "af_sky" => Ok(10),
        value => Err(NeuralError::InvalidValue {
            value: value.to_string(),
            expected: "one of the kokoro-en-v0_19 speaker ids".to_string(),
        }),
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn pcm_audio_chunk(sequence: u64, samples: &[f32]) -> Text2VoiceStreamEvent {
    Text2VoiceStreamEvent::AudioChunk {
        sequence,
        audio: encode_pcm16(samples),
        media_type: "audio/pcm;rate=24000;format=s16le".to_string(),
    }
}

fn encode_pcm16(samples: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    samples
        .iter()
        .for_each(|sample| push_pcm16(&mut bytes, *sample));
    bytes
}

fn encode_pcm16_wav(samples: &[f32]) -> Vec<u8> {
    let data_size = samples.len() as u32 * 2;
    let mut bytes = Vec::with_capacity(44 + data_size as usize);
    push_wav_header(&mut bytes, data_size);
    samples
        .iter()
        .for_each(|sample| push_pcm16(&mut bytes, *sample));
    bytes
}

fn push_wav_header(bytes: &mut Vec<u8>, data_size: u32) {
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&KOKOROS_SAMPLE_RATE_HZ.to_le_bytes());
    bytes.extend_from_slice(&(KOKOROS_SAMPLE_RATE_HZ * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_size.to_le_bytes());
}

fn push_pcm16(bytes: &mut Vec<u8>, sample: f32) {
    let clipped = sample.clamp(-1.0, 1.0);
    let scaled = (clipped * i16::MAX as f32).round() as i16;
    bytes.extend_from_slice(&scaled.to_le_bytes());
}

fn sherpa_error(engine_id: &str, message: &str) -> NeuralError {
    NeuralError::ProviderFailed {
        provider_id: engine_id.to_string(),
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_pcm16_wav_writes_riff_header_and_clips_samples() {
        let bytes = encode_pcm16_wav(&[-2.0, 0.0, 2.0]);

        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 6);
        assert_eq!(
            i16::from_le_bytes(bytes[44..46].try_into().unwrap()),
            i16::MIN + 1
        );
        assert_eq!(i16::from_le_bytes(bytes[46..48].try_into().unwrap()), 0);
        assert_eq!(
            i16::from_le_bytes(bytes[48..50].try_into().unwrap()),
            i16::MAX
        );
    }

    #[test]
    fn pcm_stream_emitter_preserves_independent_callback_blocks() {
        let mut emitter = PcmStreamEmitter::new();

        let first = emitter.callback_event(&[0.0, 0.25, 0.5]).unwrap();
        let shorter_second = emitter.callback_event(&[0.75, 1.0]).unwrap();

        assert_eq!(audio_bytes(first), encode_pcm16(&[0.0, 0.25, 0.5]));
        assert_eq!(audio_bytes(shorter_second), encode_pcm16(&[0.75, 1.0]));
    }

    #[test]
    fn pcm_stream_emitter_uses_final_audio_when_callbacks_are_absent() {
        let mut emitter = PcmStreamEmitter::new();

        let complete_audio = emitter.final_tail_event(&[0.0, 0.25]).unwrap();

        assert_eq!(audio_bytes(complete_audio), encode_pcm16(&[0.0, 0.25]));
    }

    #[test]
    fn pcm_stream_emitter_flushes_only_an_unsent_final_tail() {
        let mut emitter = PcmStreamEmitter::new();

        emitter.callback_event(&[0.0, 0.25]).unwrap();
        let tail = emitter.final_tail_event(&[0.0, 0.25, 0.5, 0.75]).unwrap();

        assert_eq!(audio_bytes(tail), encode_pcm16(&[0.5, 0.75]));
    }

    fn audio_bytes(event: Text2VoiceStreamEvent) -> Vec<u8> {
        let Text2VoiceStreamEvent::AudioChunk {
            audio, media_type, ..
        } = event
        else {
            panic!("expected audio chunk");
        };
        assert_eq!(media_type, "audio/pcm;rate=24000;format=s16le");
        audio
    }

    #[test]
    fn validate_config_rejects_non_positive_speed() {
        let config = KokorosText2VoiceConfig {
            engine_id: "kokoros".to_string(),
            model_path: "model.onnx".to_string(),
            voices_path: "voices.bin".to_string(),
            default_voice_id: "af_heart".to_string(),
            speed: 0.0,
        };

        let error = validate_config(&config).unwrap_err();

        assert!(error.to_string().contains("greater than zero"));
    }

    #[test]
    fn voice_id_to_sid_maps_kokoro_en_v019_names() {
        assert_eq!(voice_id_to_sid("af_alloy").unwrap(), 0);
        assert_eq!(voice_id_to_sid("af_heart").unwrap(), 3);
        assert_eq!(voice_id_to_sid("af_sky").unwrap(), 10);
    }

    #[test]
    fn voice_id_to_sid_rejects_unknown_voice() {
        let error = voice_id_to_sid("am_adam").unwrap_err();

        assert!(error.to_string().contains("kokoro-en-v0_19"));
    }
}
