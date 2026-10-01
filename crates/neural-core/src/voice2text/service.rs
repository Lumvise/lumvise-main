use crate::Result;
use crate::config::EngineConfig;
#[cfg(feature = "whisper-rs")]
use crate::deferred_engine::DeferredEngine;
use crate::process::StreamControl;
use crate::voice2text::spawned_engine::SpawnedVoice2TextEngine;
use crate::voice2text::{Voice2TextRequest, Voice2TextResponse, Voice2TextStreamEvent};
#[cfg(feature = "whisper-rs")]
use crate::voice2text::{WhisperRsVoice2TextConfig, WhisperRsVoice2TextEngine};

#[cfg(feature = "whisper-rs")]
type WhisperRsEngineFactory = Box<dyn Fn() -> Result<WhisperRsVoice2TextEngine> + Send + Sync>;

pub struct Voice2TextService {
    engine: Voice2TextEngine,
}

enum Voice2TextEngine {
    Spawned(SpawnedVoice2TextEngine),
    #[cfg(feature = "whisper-rs")]
    WhisperRs(DeferredEngine<WhisperRsVoice2TextEngine, WhisperRsEngineFactory>),
}

impl Voice2TextService {
    /// Creates a voice-to-text service backed by a spawned engine.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_neural_core::{EngineConfig, SpawnConfig, Voice2TextService};
    /// let config = EngineConfig {
    ///     engine_id: "stt".into(),
    ///     spawn: SpawnConfig { command: "echo".into(), args: vec![], timeout_ms: 1000 },
    ///     expected_dimensions: None,
    /// };
    /// assert!(Voice2TextService::new(config).is_ok());
    /// ```
    pub fn new(config: EngineConfig) -> Result<Self> {
        Ok(Self {
            engine: Voice2TextEngine::Spawned(SpawnedVoice2TextEngine::new(config)?),
        })
    }

    #[cfg(feature = "whisper-rs")]
    /// Creates a voice-to-text service backed by the native whisper-rs engine.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let service = Voice2TextService::whisper_rs(
    ///     lumvise_neural_core::voice2text::WhisperRsVoice2TextConfig::base_english("whisper"),
    /// )?;
    /// ```
    pub fn whisper_rs(config: WhisperRsVoice2TextConfig) -> Result<Self> {
        let builder_config = config.clone();
        Ok(Self {
            engine: Voice2TextEngine::WhisperRs(DeferredEngine::new(
                config.engine_id.clone(),
                Box::new(move || WhisperRsVoice2TextEngine::new(builder_config.clone())),
            )),
        })
    }

    /// Starts model/model-loader initialization before the first request.
    pub fn warmup(&self) -> Result<()> {
        match &self.engine {
            Voice2TextEngine::Spawned(_) => Ok(()),
            #[cfg(feature = "whisper-rs")]
            Voice2TextEngine::WhisperRs(engine) => engine.warmup(),
        }
    }

    pub fn transcribe(&self, request: &Voice2TextRequest) -> Result<Voice2TextResponse> {
        match &self.engine {
            Voice2TextEngine::Spawned(engine) => engine.transcribe(request),
            #[cfg(feature = "whisper-rs")]
            Voice2TextEngine::WhisperRs(engine) => {
                let engine = engine.get_ready_for_request()?;
                engine.transcribe(request)
            }
        }
    }

    pub fn stream(
        &self,
        request: &Voice2TextRequest,
        control: StreamControl,
    ) -> Result<Vec<Voice2TextStreamEvent>> {
        match &self.engine {
            Voice2TextEngine::Spawned(engine) => engine.stream(request, control),
            #[cfg(feature = "whisper-rs")]
            Voice2TextEngine::WhisperRs(engine) => {
                let engine = engine.get_ready_for_request()?;
                engine.stream(request)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "whisper-rs")]
    #[test]
    fn whisper_rs_constructor_is_lazy() {
        let mut config = crate::voice2text::WhisperRsVoice2TextConfig::base_english("lazy-stt");
        config.model_path = "/tmp/does-not-exist-while-testing-whisper".into();

        let service = Voice2TextService::whisper_rs(config).unwrap();

        assert!(service.warmup().is_err());
    }
}
