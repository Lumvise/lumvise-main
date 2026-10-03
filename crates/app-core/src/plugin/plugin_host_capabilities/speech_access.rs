//! Owns speech availability for desktop and plugin callers. Installed models
//! stay resident when disabled, allowing preferences to change without a download.
//! Call the available_speech_* methods; raw service slots remain internal.
use super::*;

impl PluginHostServices {
    pub(crate) fn available_speech_recognizer(
        &self,
    ) -> Result<Option<Arc<dyn SpeechRecognizer>>, HostCapabilityError> {
        let frontend = self
            .frontend
            .lock()
            .map_err(|_| failed(SPEECH_TO_TEXT, "frontend mutex poisoned"))?;
        if !frontend.app_settings().speech_recognition_enabled {
            return Ok(None);
        }
        drop(frontend);
        self.speech_recognizer
            .lock()
            .map(|slot| slot.clone())
            .map_err(|_| failed(SPEECH_TO_TEXT, "speech recognizer slot mutex poisoned"))
    }

    pub(crate) fn available_speech_synthesizer(
        &self,
    ) -> Result<Option<Arc<dyn SpeechSynthesizer>>, HostCapabilityError> {
        let frontend = self
            .frontend
            .lock()
            .map_err(|_| failed(TEXT_TO_SPEECH, "frontend mutex poisoned"))?;
        if !frontend.app_settings().speech_synthesis_enabled {
            return Ok(None);
        }
        drop(frontend);
        self.speech_synthesizer
            .lock()
            .map(|slot| slot.clone())
            .map_err(|_| failed(TEXT_TO_SPEECH, "speech synthesizer slot mutex poisoned"))
    }

    /// Which speech dependencies a voice session needs and whether each is
    /// installed. Reads the same recognizer/synthesizer slots the turn-time
    /// `modalities.*` capabilities read, so availability and turn behaviour
    /// cannot drift (issue #92).
    ///
    /// Reports names only, never model ids or service handles, so callers learn
    /// what to configure without seeing engine internals.
    pub(super) fn speech_availability(&self) -> Result<Value, HostCapabilityError> {
        let recognizer_installed = self.available_speech_recognizer()?.is_some();
        let synthesizer_installed = self.available_speech_synthesizer()?.is_some();
        let mut missing = Vec::new();
        if !recognizer_installed {
            missing.push("Speech-to-Text model");
        }
        if !synthesizer_installed {
            missing.push("Text-to-Speech voice");
        }
        Ok(json!({
            "available": missing.is_empty(),
            "speech_to_text": recognizer_installed,
            "text_to_speech": synthesizer_installed,
            "missing": missing,
        }))
    }

    pub(super) fn speech_to_text_executor(
        &self,
    ) -> Result<Arc<SpeechToTextExecutor>, HostCapabilityError> {
        let service = self.available_speech_recognizer()?.ok_or_else(|| {
            unavailable(
                SPEECH_TO_TEXT,
                "speech recognition is disabled or no recognizer is installed",
            )
        })?;
        let mut slot = self
            .speech_to_text_executor
            .lock()
            .map_err(|_| failed(SPEECH_TO_TEXT, "speech executor slot mutex poisoned"))?;
        if let Some((current_service, executor)) = slot.as_ref()
            && Arc::ptr_eq(current_service, &service)
        {
            return Ok(Arc::clone(executor));
        }
        let executor = Arc::new(SpeechToTextExecutor::start(Arc::clone(&service)));
        *slot = Some((service, Arc::clone(&executor)));
        Ok(executor)
    }

    pub(super) fn text_to_speech_executor(
        &self,
    ) -> Result<Arc<TextToSpeechExecutor>, HostCapabilityError> {
        let service = self.available_speech_synthesizer()?.ok_or_else(|| {
            unavailable(
                TEXT_TO_SPEECH,
                "speech synthesis is disabled or no synthesizer is installed",
            )
        })?;
        let mut slot = self
            .text_to_speech_executor
            .lock()
            .map_err(|_| failed(TEXT_TO_SPEECH, "speech executor slot mutex poisoned"))?;
        if let Some((current_service, executor)) = slot.as_ref()
            && Arc::ptr_eq(current_service, &service)
        {
            return Ok(Arc::clone(executor));
        }
        let executor = Arc::new(TextToSpeechExecutor::start(Arc::clone(&service)));
        *slot = Some((service, Arc::clone(&executor)));
        Ok(executor)
    }
}
