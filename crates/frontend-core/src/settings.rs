use crate::state::FrontendCore;
use crate::types::DashboardView;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AudioDeviceCatalog {
    pub inputs: Vec<AudioDeviceOption>,
    pub outputs: Vec<AudioDeviceOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDeviceOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListeningMode {
    Balanced,
    Thinking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantEngine {
    NativeMcp,
    Cerebras,
    OpenRouter,
    Codex,
    Claude,
    Gemini,
    OpenAiRealtime,
    ZAi,
    #[serde(rename = "custom_openai")]
    CustomOpenAi,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AssistantProviderCatalog {
    pub providers: Vec<AssistantProviderOption>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantModelSource {
    Api,
    Client,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantProviderOption {
    pub id: String,
    pub label: String,
    pub models: Vec<AssistantModelOption>,
    pub default_model: Option<String>,
    pub model_source: AssistantModelSource,
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantModelOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub setup_completed: bool,
    /// Prefer document layout and OCR models when available; basic conversion stays usable.
    #[serde(default = "default_document_enhancement_enabled")]
    pub document_enhancement_enabled: bool,
    #[serde(default = "default_speech_enabled")]
    pub speech_recognition_enabled: bool,
    #[serde(default = "default_speech_enabled")]
    pub speech_synthesis_enabled: bool,
    #[serde(default = "default_bulb_visible")]
    pub bulb_visible: bool,
    pub input_device_id: Option<String>,
    pub output_device_id: Option<String>,
    pub listening_mode: ListeningMode,
    pub assistant_engine: AssistantEngine,
    pub assistant_model: Option<String>,
    #[serde(default)]
    pub assistant_direct_audio: bool,
    /// Knowledge generation overrides: `None` uses the assistant engine.
    #[serde(default)]
    pub generation_engine: Option<AssistantEngine>,
    #[serde(default)]
    pub generation_model: Option<String>,
    pub default_dashboard_view: DashboardView,
    pub graph_view_enabled: bool,
    pub voice_recording_enabled: bool,
    pub desktop_broadcasts_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppSettingsPatch {
    SetupCompleted(bool),
    DocumentEnhancementEnabled(bool),
    SpeechRecognitionEnabled(bool),
    SpeechSynthesisEnabled(bool),
    BulbVisible(bool),
    InputDevice(Option<String>),
    OutputDevice(Option<String>),
    ListeningMode(ListeningMode),
    AssistantEngine(AssistantEngine),
    AssistantModel(Option<String>),
    AssistantDirectAudio(bool),
    GenerationEngine(Option<AssistantEngine>),
    GenerationModel(Option<String>),
    DefaultDashboardView(DashboardView),
    GraphViewEnabled(bool),
    VoiceRecordingEnabled(bool),
    DesktopBroadcastsEnabled(bool),
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            setup_completed: false,
            document_enhancement_enabled: true,
            speech_recognition_enabled: true,
            speech_synthesis_enabled: true,
            bulb_visible: true,
            input_device_id: None,
            output_device_id: None,
            listening_mode: ListeningMode::Balanced,
            assistant_engine: AssistantEngine::NativeMcp,
            assistant_model: None,
            assistant_direct_audio: false,
            generation_engine: None,
            generation_model: None,
            default_dashboard_view: DashboardView::CanvasDashboard,
            graph_view_enabled: true,
            voice_recording_enabled: true,
            desktop_broadcasts_enabled: true,
        }
    }
}

impl AppSettings {
    /// Hydrates durable settings from a renderer value. Obsolete completion
    /// and Assistant Surface keys are intentionally ignored.
    pub fn from_renderer_value(value: &Value) -> Self {
        let mut settings = Self {
            setup_completed: value["setupCompleted"].as_bool().unwrap_or(false),
            document_enhancement_enabled: value["documentEnhancementEnabled"]
                .as_bool()
                .unwrap_or(true),
            speech_recognition_enabled: value["speechRecognitionEnabled"].as_bool().unwrap_or(true),
            speech_synthesis_enabled: value["speechSynthesisEnabled"].as_bool().unwrap_or(true),
            input_device_id: optional_string_field(value, "inputDeviceId"),
            output_device_id: optional_string_field(value, "outputDeviceId"),
            ..Default::default()
        };
        settings.listening_mode = match string_field(value, "listeningMode").as_deref() {
            Some("thinking") => ListeningMode::Thinking,
            _ => ListeningMode::Balanced,
        };
        settings.assistant_direct_audio = value["assistantDirectAudio"].as_bool().unwrap_or(false);
        settings.bulb_visible = value["bulbVisible"].as_bool().unwrap_or(true);
        settings.assistant_engine =
            AssistantEngine::parse(string_field(value, "assistantEngine").as_deref());
        settings.assistant_model = settings
            .assistant_engine
            .model_field()
            .and_then(|field| optional_string_field(value, field))
            .or_else(|| optional_string_field(value, "assistantModel"));
        settings.generation_engine =
            string_field(value, "generationEngine")
                .as_deref()
                .and_then(|engine| {
                    (engine != "native_mcp").then(|| AssistantEngine::parse(Some(engine)))
                });
        settings.generation_model = optional_string_field(value, "generationModel");
        settings
    }

    /// Applies one durable App Settings patch and returns the renderer projection.
    pub fn apply_app_settings_patch(&mut self, patch: &AppSettingsPatch) -> Value {
        match patch {
            AppSettingsPatch::DocumentEnhancementEnabled(value) => {
                self.document_enhancement_enabled = *value;
                json!({ "documentEnhancementEnabled": value })
            }
            AppSettingsPatch::SetupCompleted(value) => {
                self.setup_completed = *value;
                json!({ "setupCompleted": value })
            }
            AppSettingsPatch::SpeechRecognitionEnabled(value) => {
                self.speech_recognition_enabled = *value;
                json!({ "speechRecognitionEnabled": value })
            }
            AppSettingsPatch::SpeechSynthesisEnabled(value) => {
                self.speech_synthesis_enabled = *value;
                json!({ "speechSynthesisEnabled": value })
            }
            AppSettingsPatch::BulbVisible(value) => {
                self.bulb_visible = *value;
                json!({ "bulbVisible": value })
            }
            AppSettingsPatch::InputDevice(value) => {
                self.input_device_id = value.clone();
                json!({ "inputDeviceId": value })
            }
            AppSettingsPatch::OutputDevice(value) => {
                self.output_device_id = value.clone();
                json!({ "outputDeviceId": value })
            }
            AppSettingsPatch::ListeningMode(value) => {
                self.listening_mode = *value;
                json!({ "listeningMode": value.value() })
            }
            AppSettingsPatch::AssistantEngine(value) => {
                self.assistant_engine = *value;
                json!({
                    "assistantEngine": value.value(),
                    "assistantModel": self.active_model(),
                })
            }
            AppSettingsPatch::AssistantDirectAudio(value) => {
                self.assistant_direct_audio = *value;
                json!({"assistantDirectAudio": value})
            }
            AppSettingsPatch::AssistantModel(value) => {
                self.assistant_model = value.clone();
                let Some(model_field) = self.assistant_engine.model_field() else {
                    return json!({ "assistantModel": "" });
                };
                json!({ "assistantModel": value, model_field: value })
            }
            AppSettingsPatch::GenerationEngine(value) => {
                if value.is_none() || self.generation_engine != *value {
                    self.generation_model = None;
                }
                self.generation_engine = *value;
                json!({
                    "generationEngine": value.map(|engine| engine.value()),
                    "generationModel": self.generation_model,
                })
            }
            AppSettingsPatch::GenerationModel(value) => {
                self.generation_model = value.clone();
                json!({ "generationModel": value })
            }
            AppSettingsPatch::DefaultDashboardView(value) => {
                self.default_dashboard_view = value.clone();
                json!({ "defaultDashboardView": value })
            }
            AppSettingsPatch::GraphViewEnabled(value) => {
                self.graph_view_enabled = *value;
                json!({ "graphViewEnabled": value })
            }
            AppSettingsPatch::VoiceRecordingEnabled(value) => {
                self.voice_recording_enabled = *value;
                json!({ "voiceRecordingEnabled": value })
            }
            AppSettingsPatch::DesktopBroadcastsEnabled(value) => {
                self.desktop_broadcasts_enabled = *value;
                json!({ "desktopBroadcastsEnabled": value })
            }
        }
    }

    fn active_model(&self) -> Option<&String> {
        match self.assistant_engine {
            AssistantEngine::NativeMcp => None,
            _ => self.assistant_model.as_ref(),
        }
    }
}

impl ListeningMode {
    pub fn value(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::Thinking => "thinking",
        }
    }
}

impl AssistantEngine {
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("cerebras") => Self::Cerebras,
            Some("openrouter") => Self::OpenRouter,
            Some("codex") => Self::Codex,
            Some("claude") => Self::Claude,
            Some("gemini") => Self::Gemini,
            Some("openai_realtime") => Self::OpenAiRealtime,
            Some("z_ai") => Self::ZAi,
            Some("custom_openai") => Self::CustomOpenAi,
            _ => Self::NativeMcp,
        }
    }

    pub fn value(self) -> &'static str {
        match self {
            Self::NativeMcp => "native_mcp",
            Self::Cerebras => "cerebras",
            Self::OpenRouter => "openrouter",
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::OpenAiRealtime => "openai_realtime",
            Self::ZAi => "z_ai",
            Self::CustomOpenAi => "custom_openai",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::NativeMcp => "Native MCP",
            Self::Cerebras => "Cerebras",
            Self::OpenRouter => "OpenRouter",
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Gemini => "Gemini",
            Self::OpenAiRealtime => "OpenAI Realtime",
            Self::ZAi => "Z.AI GLM",
            Self::CustomOpenAi => "Custom (OpenAI-compatible)",
        }
    }

    pub fn menu_id(self) -> &'static str {
        match self {
            Self::NativeMcp => "engine:native_mcp",
            Self::Cerebras => "engine:cerebras",
            Self::OpenRouter => "engine:openrouter",
            Self::Codex => "engine:codex",
            Self::Claude => "engine:claude",
            Self::Gemini => "engine:gemini",
            Self::OpenAiRealtime => "engine:openai_realtime",
            Self::ZAi => "engine:z_ai",
            Self::CustomOpenAi => "engine:custom_openai",
        }
    }

    pub fn model_field(self) -> Option<&'static str> {
        match self {
            Self::NativeMcp => None,
            Self::Cerebras => Some("cerebrasModel"),
            Self::OpenRouter => Some("openRouterModel"),
            Self::Codex => Some("codexModel"),
            Self::Claude => Some("claudeModel"),
            Self::Gemini => Some("geminiModel"),
            Self::OpenAiRealtime => Some("openAiRealtimeModel"),
            Self::ZAi => Some("zAiModel"),
            Self::CustomOpenAi => Some("customOpenAiModel"),
        }
    }

    pub fn accepts_local_api_key(self) -> bool {
        matches!(
            self,
            Self::Cerebras
                | Self::OpenRouter
                | Self::ZAi
                | Self::CustomOpenAi
                | Self::Gemini
                | Self::OpenAiRealtime
        )
    }
}

impl FrontendCore {
    /// Applies one App Settings patch.
    pub fn apply_app_settings_patch(&mut self, patch: &AppSettingsPatch) -> Value {
        self.settings.apply_app_settings_patch(patch)
    }

    /// Returns current App Settings.
    pub fn app_settings(&self) -> &AppSettings {
        &self.settings
    }

    /// Hydrates the desktop projection without changing active windows or sessions.
    /// Example: `frontend.restore_app_settings(saved_preferences)` after startup.
    pub fn restore_app_settings(&mut self, settings: AppSettings) {
        self.settings = settings;
    }

    pub fn assistant_provider_catalog(&self) -> &AssistantProviderCatalog {
        &self.assistant_provider_catalog
    }

    /// Replaces the active-source catalog and reports whether source-local
    /// reconciliation filled an unset model. Explicit choices survive discovery.
    /// Example: `frontend.replace_assistant_provider_catalog(discovered_catalog)`.
    pub fn replace_assistant_provider_catalog(
        &mut self,
        catalog: AssistantProviderCatalog,
    ) -> bool {
        self.assistant_provider_catalog = catalog;
        self.reconcile_assistant_selection()
    }

    fn reconcile_assistant_selection(&mut self) -> bool {
        let previous = self.settings.assistant_model.clone();
        if previous
            .as_deref()
            .is_some_and(|model| !model.trim().is_empty())
        {
            return false;
        }
        let provider_id = self.settings.assistant_engine.value();
        let providers = &self.assistant_provider_catalog.providers;
        let Some(provider) = providers.iter().find(|provider| provider.id == provider_id) else {
            return false;
        };
        self.settings.assistant_model = initial_catalog_model(provider);
        self.settings.assistant_model != previous
    }
}

fn initial_catalog_model(provider: &AssistantProviderOption) -> Option<String> {
    let default = provider.default_model.as_ref().and_then(|default| {
        provider
            .models
            .iter()
            .find(|model| model.id == *default && !model.id.trim().is_empty())
    });
    default
        .or_else(|| {
            provider
                .models
                .iter()
                .find(|model| !model.id.trim().is_empty())
        })
        .map(|model| model.id.clone())
}

fn default_bulb_visible() -> bool {
    true
}

fn default_speech_enabled() -> bool {
    true
}

fn default_document_enhancement_enabled() -> bool {
    true
}

fn optional_string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_enhancement_defaults_on_and_preserves_explicit_opt_out() {
        let mut value = serde_json::to_value(AppSettings::default()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("document_enhancement_enabled");
        let mut settings: AppSettings = serde_json::from_value(value).unwrap();
        assert!(settings.document_enhancement_enabled);
        assert_eq!(
            settings.apply_app_settings_patch(&AppSettingsPatch::DocumentEnhancementEnabled(false)),
            json!({"documentEnhancementEnabled": false})
        );
        let restored: AppSettings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        assert!(!restored.document_enhancement_enabled);
        assert!(
            !AppSettings::from_renderer_value(&json!({"documentEnhancementEnabled": false}))
                .document_enhancement_enabled
        );
        assert!(AppSettings::from_renderer_value(&json!({})).document_enhancement_enabled);
    }

    #[test]
    fn bulb_visibility_defaults_visible_and_round_trips_when_hidden() {
        let mut value = serde_json::to_value(AppSettings::default()).unwrap();
        value.as_object_mut().unwrap().remove("bulb_visible");
        let mut settings: AppSettings = serde_json::from_value(value).unwrap();
        assert!(settings.bulb_visible);
        assert_eq!(
            settings.apply_app_settings_patch(&AppSettingsPatch::BulbVisible(false)),
            json!({"bulbVisible": false})
        );
        let restored: AppSettings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        assert!(!restored.bulb_visible);
        assert!(!AppSettings::from_renderer_value(&json!({"bulbVisible": false})).bulb_visible);
        assert!(AppSettings::from_renderer_value(&json!({})).bulb_visible);
    }

    #[test]
    fn old_settings_json_without_generation_fields_deserializes_to_none() {
        let settings: AppSettings = serde_json::from_value(json!({
            "input_device_id": null,
            "output_device_id": null,
            "listening_mode": "balanced",
            "assistant_engine": "cerebras",
            "assistant_model": "assistant-model",
            "assistant_direct_audio": false,
            "default_dashboard_view": "canvas_dashboard",
            "graph_view_enabled": true,
            "voice_recording_enabled": true,
            "desktop_broadcasts_enabled": true
        }))
        .unwrap();

        assert_eq!(settings.generation_engine, None);
        assert_eq!(settings.generation_model, None);
    }

    #[test]
    fn generation_settings_round_trip_and_same_as_assistant_clears_model() {
        let mut settings = AppSettings {
            assistant_engine: AssistantEngine::Codex,
            assistant_model: Some("assistant-model".into()),
            ..Default::default()
        };
        assert_eq!(
            settings.apply_app_settings_patch(&AppSettingsPatch::GenerationEngine(Some(
                AssistantEngine::Cerebras
            ))),
            json!({"generationEngine": "cerebras", "generationModel": null})
        );
        assert_eq!(
            settings.apply_app_settings_patch(&AppSettingsPatch::GenerationModel(Some(
                "gen-model".into()
            ))),
            json!({"generationModel": "gen-model"})
        );
        let stored = serde_json::to_value(&settings).unwrap();
        let reloaded: AppSettings = serde_json::from_value(stored).unwrap();
        assert_eq!(reloaded, settings);

        assert_eq!(
            settings.apply_app_settings_patch(&AppSettingsPatch::GenerationEngine(None)),
            json!({"generationEngine": null, "generationModel": null})
        );
        assert_eq!(settings.generation_model, None);
    }

    fn codex_catalog(models: &[&str], default: Option<&str>) -> AssistantProviderCatalog {
        AssistantProviderCatalog {
            providers: vec![AssistantProviderOption {
                id: "codex".into(),
                label: "Codex".into(),
                available: true,
                model_source: AssistantModelSource::Client,
                default_model: default.map(str::to_string),
                models: models
                    .iter()
                    .map(|id| AssistantModelOption {
                        id: (*id).into(),
                        label: (*id).into(),
                    })
                    .collect(),
            }],
        }
    }

    fn codex_frontend(model: Option<&str>) -> FrontendCore {
        FrontendCore::new(AppSettings {
            assistant_engine: AssistantEngine::Codex,
            assistant_model: model.map(str::to_string),
            ..Default::default()
        })
    }

    #[test]
    fn catalog_publication_preserves_explicit_models_even_when_unavailable() {
        let mut unavailable = codex_catalog(&["advertised-model"], Some("advertised-model"));
        unavailable.providers[0].available = false;
        for catalog in [
            AssistantProviderCatalog::default(),
            unavailable,
            codex_catalog(&["advertised-model"], Some("advertised-model")),
            codex_catalog(&["saved-model"], Some("saved-model")),
        ] {
            let mut frontend = codex_frontend(Some("saved-model"));
            assert!(!frontend.replace_assistant_provider_catalog(catalog.clone()));
            assert_eq!(
                frontend.app_settings().assistant_model.as_deref(),
                Some("saved-model")
            );
            assert_eq!(frontend.assistant_provider_catalog(), &catalog);
        }
    }

    #[test]
    fn catalog_publication_fills_unset_models_from_advertised_choices_only() {
        for unset in [None, Some(""), Some("  ")] {
            for default in [Some("preferred-model"), Some("unadvertised-model"), None] {
                let mut frontend = codex_frontend(unset);
                let catalog = codex_catalog(&["first-model", "preferred-model"], default);
                assert!(frontend.replace_assistant_provider_catalog(catalog));
                let expected = if default == Some("preferred-model") {
                    "preferred-model"
                } else {
                    "first-model"
                };
                assert_eq!(
                    frontend.app_settings().assistant_model.as_deref(),
                    Some(expected)
                );
            }
        }
    }

    #[test]
    fn empty_catalog_cannot_fill_an_unset_model_from_a_phantom_default() {
        for models in [&[][..], &[""][..], &["  "][..]] {
            let mut frontend = codex_frontend(None);
            assert!(
                !frontend.replace_assistant_provider_catalog(codex_catalog(
                    models,
                    Some("phantom-model")
                ))
            );
            assert_eq!(frontend.app_settings().assistant_model, None);
        }
    }

    #[test]
    fn generation_engine_changes_clear_models_but_repeated_selection_keeps_them() {
        for (next, expected) in [
            (Some(AssistantEngine::Codex), Some("gen-model")),
            (Some(AssistantEngine::Cerebras), None),
            (None, None),
        ] {
            let mut settings = AppSettings {
                generation_engine: Some(AssistantEngine::Codex),
                generation_model: Some("gen-model".into()),
                ..Default::default()
            };
            let patch =
                settings.apply_app_settings_patch(&AppSettingsPatch::GenerationEngine(next));
            assert_eq!(settings.generation_engine, next);
            assert_eq!(settings.generation_model.as_deref(), expected);
            assert_eq!(
                patch,
                json!({"generationEngine": next.map(|engine| engine.value()), "generationModel": expected})
            );
        }
    }

    #[test]
    fn same_as_assistant_clears_an_orphan_generation_model() {
        let mut settings = AppSettings {
            generation_model: Some("gen-model".into()),
            ..Default::default()
        };
        settings.apply_app_settings_patch(&AppSettingsPatch::GenerationEngine(None));
        assert_eq!(settings.generation_engine, None);
        assert_eq!(settings.generation_model, None);
    }

    #[test]
    fn custom_openai_engine_round_trips_parse_value_and_model_field() {
        assert_eq!(
            AssistantEngine::parse(Some("custom_openai")),
            AssistantEngine::CustomOpenAi
        );
        assert_eq!(AssistantEngine::CustomOpenAi.value(), "custom_openai");
        assert_eq!(
            AssistantEngine::CustomOpenAi.label(),
            "Custom (OpenAI-compatible)"
        );
        assert_eq!(
            AssistantEngine::CustomOpenAi.menu_id(),
            "engine:custom_openai"
        );
        assert_eq!(
            AssistantEngine::CustomOpenAi.model_field(),
            Some("customOpenAiModel")
        );
        assert!(AssistantEngine::CustomOpenAi.accepts_local_api_key());

        let stored: AppSettings = serde_json::from_value(json!({
            "input_device_id": null,
            "output_device_id": null,
            "listening_mode": "balanced",
            "assistant_engine": "custom_openai",
            "assistant_model": "qwen3",
            "assistant_direct_audio": false,
            "default_dashboard_view": "canvas_dashboard",
            "graph_view_enabled": true,
            "voice_recording_enabled": true,
            "desktop_broadcasts_enabled": true
        }))
        .unwrap_or_else(|error| {
            panic!("settings must deserialize: {error}");
        });
        assert_eq!(stored.assistant_engine, AssistantEngine::CustomOpenAi);

        let mut settings = AppSettings {
            assistant_engine: AssistantEngine::CustomOpenAi,
            ..Default::default()
        };
        assert_eq!(
            settings
                .apply_app_settings_patch(&AppSettingsPatch::AssistantModel(Some("qwen3".into()))),
            json!({"assistantModel": "qwen3", "customOpenAiModel": "qwen3"})
        );
        assert_eq!(
            serde_json::to_value(&settings).unwrap()["assistant_engine"],
            json!("custom_openai")
        );
    }
}
