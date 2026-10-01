//! Wires the shared managed-model lifecycle
//! (`lumvise_neural_core::managed_models`) into App Core's live vector and
//! speech runtime.
//!
//! This module owns the one production [`ModelRuntimeAdapter`]. It never
//! downloads, validates, or publishes assets itself; `ManagedModelManager`
//! calls `activate` only after a candidate is fully downloaded and verified,
//! and calls `persist` only after `activate` succeeds. Callers elsewhere in
//! App Core reach the manager through [`AppCore::managed_models`] and never
//! touch this adapter directly.

#[cfg(all(test, feature = "desktop-app"))]
mod desktop_vector_tests {
    use super::*;
    use lumvise_neural_core::managed_models::builtin_catalog;
    use std::sync::Arc;

    #[test]
    fn desktop_vector_activation_reaches_native_model_loader() {
        let app = Arc::new(AppCore::in_memory().unwrap());
        let adapter = AppCoreModelRuntimeAdapter::new(Arc::downgrade(&app));
        let assets = tempfile::TempDir::new().unwrap();
        let catalog = builtin_catalog();
        let bge = catalog
            .iter()
            .find(|entry| entry.id == "vector.bge-small-en-v1.5")
            .unwrap();
        let failure = adapter.activate(bge, assets.path()).unwrap_err();
        assert!(
            failure.contains("model.onnx"),
            "expected native loader failure, got {failure}"
        );
        let disabled = catalog
            .iter()
            .find(|entry| entry.disabled && entry.kind == ManagedModelKind::Vector)
            .unwrap();
        adapter.activate(disabled, assets.path()).unwrap();
    }
}

use crate::AppCore;
use lumvise_neural_core::managed_models::{
    ManagedModelCatalogEntry, ManagedModelKind, ModelRuntimeAdapter,
};
use std::path::Path;
use std::sync::Weak;

#[cfg(feature = "native-voice")]
const KOKORO_DEFAULT_VOICE: &str = "af_sky";
#[cfg(feature = "native-voice")]
const KOKORO_DEFAULT_SPEED: f32 = 0.9;

/// Production runtime adapter installed once at App Core startup.
///
/// Holds a weak reference so the manager can never keep App Core alive past
/// its own lifetime; App Core owns the manager, not the other way around.
pub(crate) struct AppCoreModelRuntimeAdapter {
    app: Weak<AppCore>,
}

impl AppCoreModelRuntimeAdapter {
    pub(crate) fn new(app: Weak<AppCore>) -> Self {
        Self { app }
    }

    fn app(&self) -> Result<std::sync::Arc<AppCore>, String> {
        self.app
            .upgrade()
            .ok_or_else(|| "App Core has shut down".to_string())
    }
}

impl ModelRuntimeAdapter for AppCoreModelRuntimeAdapter {
    fn activate(
        &self,
        entry: &ManagedModelCatalogEntry,
        published_path: &Path,
    ) -> Result<(), String> {
        let app = self.app()?;
        match entry.kind {
            ManagedModelKind::Vector => activate_vector(&app, entry, published_path),
            ManagedModelKind::SpeechToText => activate_speech_to_text(&app, entry, published_path),
            ManagedModelKind::TextToSpeech => activate_text_to_speech(&app, entry, published_path),
        }
    }

    fn persist(&self, _kind: ManagedModelKind, _model_id: &str) -> Result<(), String> {
        // `ManagedModelManager` writes its own active-selection marker after
        // `activate` succeeds; that marker is the single durable record, so
        // App Core has no separate persistence step for a successful cutover.
        Ok(())
    }
}

#[cfg(feature = "native-vector")]
fn activate_vector(
    app: &AppCore,
    entry: &ManagedModelCatalogEntry,
    published_path: &Path,
) -> Result<(), String> {
    use lumvise_neural_core::{Text2VectorRuntimeConfig, Text2VectorService};

    if entry.disabled {
        return app
            .plugin_endpoints()
            .clear_plugin_vectorizer()
            .map_err(|error| error.to_string());
    }
    let fastembed_model_id = entry.id.strip_prefix("vector.").ok_or_else(|| {
        format!(
            "managed vector model id `{}` is missing the `vector.` prefix",
            entry.id
        )
    })?;
    let service =
        Text2VectorService::from_runtime_config(Text2VectorRuntimeConfig::fastembed_local_onnx(
            &entry.id,
            fastembed_model_id,
            published_path.to_path_buf(),
        ))
        .map_err(|error| format!("loading managed vector model `{}`: {error}", entry.id))?;
    app.plugin_endpoints()
        .set_plugin_vectorizer(Box::new(service))
        .map_err(|error| error.to_string())
}

#[cfg(not(feature = "native-vector"))]
fn activate_vector(
    _app: &AppCore,
    _entry: &ManagedModelCatalogEntry,
    _published_path: &Path,
) -> Result<(), String> {
    Err("native-vector feature is disabled".to_string())
}

#[cfg(feature = "native-voice")]
fn activate_speech_to_text(
    app: &AppCore,
    entry: &ManagedModelCatalogEntry,
    published_path: &Path,
) -> Result<(), String> {
    use lumvise_neural_core::Voice2TextService;
    use lumvise_neural_core::voice2text::{
        WhisperRsVoice2TextConfig, whisper_rs_engine::WhisperRsDecodeSettings,
    };

    let model_path = published_path
        .join("model.asset")
        .to_string_lossy()
        .into_owned();
    let config = WhisperRsVoice2TextConfig {
        engine_id: entry.id.clone(),
        model_path,
        language: Some("en".to_string()),
        threads: 4,
        beam_size: 7,
        decode: WhisperRsDecodeSettings::default_quality(),
    };
    let service = Voice2TextService::whisper_rs(config).map_err(|error| {
        format!(
            "loading managed speech-to-text model `{}`: {error}",
            entry.id
        )
    })?;
    app.replace_voice2text_service(std::sync::Arc::new(service))
        .map_err(|error| error.to_string())
}

#[cfg(not(feature = "native-voice"))]
fn activate_speech_to_text(
    _app: &AppCore,
    _entry: &ManagedModelCatalogEntry,
    _published_path: &Path,
) -> Result<(), String> {
    Err("native-voice feature is disabled".to_string())
}

#[cfg(feature = "native-voice")]
fn activate_text_to_speech(
    app: &AppCore,
    entry: &ManagedModelCatalogEntry,
    published_path: &Path,
) -> Result<(), String> {
    use lumvise_neural_core::Text2VoiceService;
    use lumvise_neural_core::text2voice::KokorosText2VoiceConfig;

    let model_path = published_path
        .join("kokoro-en-v0_19")
        .join("model.onnx")
        .to_string_lossy()
        .into_owned();
    let config = KokorosText2VoiceConfig {
        engine_id: entry.id.clone(),
        model_path,
        voices_path: "voices.bin".to_string(),
        default_voice_id: KOKORO_DEFAULT_VOICE.to_string(),
        speed: KOKORO_DEFAULT_SPEED,
    };
    let service = Text2VoiceService::kokoros(config).map_err(|error| {
        format!(
            "loading managed text-to-speech model `{}`: {error}",
            entry.id
        )
    })?;
    app.replace_text2voice_service(std::sync::Arc::new(service))
        .map_err(|error| error.to_string())
}

#[cfg(not(feature = "native-voice"))]
fn activate_text_to_speech(
    _app: &AppCore,
    _entry: &ManagedModelCatalogEntry,
    _published_path: &Path,
) -> Result<(), String> {
    Err("native-voice feature is disabled".to_string())
}
