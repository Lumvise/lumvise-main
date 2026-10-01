use crate::Result;
use crate::config::EngineConfig;
#[cfg(feature = "kokoros")]
use crate::deferred_engine::DeferredEngine;
use crate::process::StreamControl;
use crate::text2voice::spawned_engine::SpawnedText2VoiceEngine;
use crate::text2voice::speech_text::prepare_text2voice_request;
#[cfg(feature = "kokoros")]
use crate::text2voice::{KokorosText2VoiceConfig, KokorosText2VoiceEngine};
use crate::text2voice::{
    Text2VoiceRequest, Text2VoiceResponse, Text2VoiceStreamEvent, Text2VoiceStreamEventSink,
};

#[cfg(feature = "kokoros")]
type KokorosEngineFactory = Box<dyn Fn() -> Result<KokorosText2VoiceEngine> + Send + Sync>;

pub struct Text2VoiceService {
    engine: Text2VoiceEngine,
}

enum Text2VoiceEngine {
    Spawned(SpawnedText2VoiceEngine),
    #[cfg(feature = "kokoros")]
    Kokoros(DeferredEngine<KokorosText2VoiceEngine, KokorosEngineFactory>),
}

impl Text2VoiceService {
    /// Creates a text-to-voice service backed by a spawned engine.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_neural_core::{EngineConfig, SpawnConfig, Text2VoiceService};
    /// let config = EngineConfig {
    ///     engine_id: "tts".into(),
    ///     spawn: SpawnConfig { command: "echo".into(), args: vec![], timeout_ms: 1000 },
    ///     expected_dimensions: None,
    /// };
    /// assert!(Text2VoiceService::new(config).is_ok());
    /// ```
    pub fn new(config: EngineConfig) -> Result<Self> {
        Ok(Self {
            engine: Text2VoiceEngine::Spawned(SpawnedText2VoiceEngine::new(config)?),
        })
    }

    #[cfg(feature = "kokoros")]
    /// Creates a text-to-voice service backed by the native Kokoros engine.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let service = Text2VoiceService::kokoros(
    ///     lumvise_neural_core::text2voice::KokorosText2VoiceConfig::kokoro_v1("tts", "af_heart"),
    /// )?;
    /// ```
    pub fn kokoros(config: KokorosText2VoiceConfig) -> Result<Self> {
        let builder_config = config.clone();
        Ok(Self {
            engine: Text2VoiceEngine::Kokoros(DeferredEngine::new(
                config.engine_id.clone(),
                Box::new(move || KokorosText2VoiceEngine::new(builder_config.clone())),
            )),
        })
    }

    /// Starts model/model-loader initialization before the first request.
    pub fn warmup(&self) -> Result<()> {
        match &self.engine {
            Text2VoiceEngine::Spawned(_) => Ok(()),
            #[cfg(feature = "kokoros")]
            Text2VoiceEngine::Kokoros(engine) => engine.warmup(),
        }
    }

    /// Synthesizes final audio for one text request.
    ///
    /// # Example
    ///
    /// ```ignore
    /// # use lumvise_neural_core::text2voice::Text2VoiceRequest;
    /// # let service = build_text2voice_service();
    /// let request = Text2VoiceRequest { text: "hello".into(), voice_id: None };
    /// let _audio = service.synthesize(&request)?;
    /// # Ok::<(), lumvise_neural_core::NeuralError>(())
    /// ```
    pub fn synthesize(&self, request: &Text2VoiceRequest) -> Result<Text2VoiceResponse> {
        let prepared = prepare_text2voice_request(request);
        match &self.engine {
            Text2VoiceEngine::Spawned(engine) => engine.synthesize(&prepared),
            #[cfg(feature = "kokoros")]
            Text2VoiceEngine::Kokoros(engine) => {
                let engine = engine.get_ready_for_request()?;
                engine.synthesize(&prepared)
            }
        }
    }

    /// Streams ordered audio chunks for one text request.
    ///
    /// # Example
    ///
    /// ```ignore
    /// # use lumvise_neural_core::process::StreamControl;
    /// # use lumvise_neural_core::text2voice::Text2VoiceRequest;
    /// # let service = build_text2voice_service();
    /// let request = Text2VoiceRequest { text: "hello".into(), voice_id: None };
    /// let _events = service.stream(&request, StreamControl::unbounded())?;
    /// # Ok::<(), lumvise_neural_core::NeuralError>(())
    /// ```
    pub fn stream(
        &self,
        request: &Text2VoiceRequest,
        control: StreamControl,
    ) -> Result<Vec<Text2VoiceStreamEvent>> {
        let mut events = Vec::new();
        self.stream_with_events(request, control, &mut |event| {
            events.push(event);
            Ok(())
        })?;
        Ok(events)
    }

    pub fn stream_with_events(
        &self,
        request: &Text2VoiceRequest,
        control: StreamControl,
        on_event: &mut Text2VoiceStreamEventSink<'_>,
    ) -> Result<()> {
        let prepared = prepare_text2voice_request(request);
        match &self.engine {
            Text2VoiceEngine::Spawned(engine) => {
                engine.stream_with_events(&prepared, control, on_event)
            }
            #[cfg(feature = "kokoros")]
            Text2VoiceEngine::Kokoros(engine) => {
                let engine = engine.get_ready_for_request()?;
                let mut guarded = |event| {
                    if control.is_cancelled() {
                        return Err(crate::NeuralError::ProviderFailed {
                            provider_id: "text2voice-stream".into(),
                            message: "stream cancelled".into(),
                        });
                    }
                    on_event(event)
                };
                engine.stream_with_events(&prepared, &mut guarded)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "kokoros")]
    #[test]
    fn kokoros_constructor_is_lazy() {
        let service = Text2VoiceService::kokoros(crate::text2voice::KokorosText2VoiceConfig {
            engine_id: "lazy-tts".into(),
            model_path: "/tmp/does-not-exist-while-testing-kokoros".into(),
            voices_path: "voices.bin".into(),
            default_voice_id: "af_heart".into(),
            speed: 1.0,
        })
        .unwrap();

        assert!(service.warmup().is_err());
    }
}
