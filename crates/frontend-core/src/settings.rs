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
                self.generation_engine = *value;
                if value.is_none() {
                    self.generation_model = None;
                }
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

    pub fn assistant_provider_catalog(&self) -> &AssistantProviderCatalog {
        &self.assistant_provider_catalog
    }

    /// Replaces the active-source catalog and reports whether source-local
    /// reconciliation changed the persisted provider/model selection.
    pub fn replace_assistant_provider_catalog(
        &mut self,
        catalog: AssistantProviderCatalog,
    ) -> bool {
        self.assistant_provider_catalog = catalog;
        self.reconcile_assistant_selection()
    }

    fn reconcile_assistant_selection(&mut self) -> bool {
        let previous = self.settings.assistant_model.clone();
        let provider_id = self.settings.assistant_engine.value();
        let Some(provider) = self
            .assistant_provider_catalog
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)
        else {
            self.settings.assistant_model = None;
            return self.settings.assistant_model != previous;
        };
        if self.settings.assistant_model.as_ref().is_none_or(|model| {
            !provider
                .models
                .iter()
                .any(|candidate| candidate.id == *model)
        }) {
            self.settings.assistant_model = provider.default_model.clone();
        }
        self.settings.assistant_model != previous
    }
}

fn default_bulb_visible() -> bool {
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
