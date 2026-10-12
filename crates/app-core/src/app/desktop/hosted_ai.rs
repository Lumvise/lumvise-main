//! Serves speech recognition and synthesis from the product's hosted AI account
//! when the user chose the hosted service, and puts the local model back otherwise.
//! The product supplies only an account; the adapters and the fallback live here.

use std::sync::Arc;

use lumvise_frontend_core::{AppSettings, AppSettingsPatch, AssistantEngine, HostedAiAccount};
use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmProvider};
use lumvise_neural_core::llm_providers::custom_openai::CustomOpenAiProvider;
use lumvise_neural_core::llm_providers::http_client::ReqwestLlmHttpClient;
use lumvise_neural_core::managed_models::{ManagedModelKind, ManagedModelState};
use lumvise_neural_core::{
    BearerTokenSource, LlmProviderConfig, LlmProviderKind, OpenRouterSpeechConfig,
    OpenRouterSpeechRecognizer, OpenRouterSpeechSynthesizer, SpeechRecognizer, SpeechSynthesizer,
    TokenInjectingClient,
};

use super::AppCoreDesktopBridge;
use crate::AppCore;

struct AccountTokens(Arc<dyn HostedAiAccount>);

impl BearerTokenSource for AccountTokens {
    fn bearer_token(&self, force_refresh: bool) -> Result<String, String> {
        self.0.bearer_token(force_refresh)
    }
}

/// The hosted adapters of one account and which speech ones own their slot now.
pub(super) struct HostedAi {
    llm: Arc<dyn LlmProvider>,
    recognizer: Arc<dyn SpeechRecognizer>,
    synthesizer: Arc<dyn SpeechSynthesizer>,
    recognizer_installed: bool,
    synthesizer_installed: bool,
}

impl HostedAi {
    /// Builds the adapters once, e.g. `HostedAi::new(account)`; nothing is installed yet.
    pub(super) fn new(account: Arc<dyn HostedAiAccount>) -> Result<Self, String> {
        let endpoint = account.endpoint();
        let client: Arc<dyn LlmHttpClient> = Arc::new(TokenInjectingClient::new(
            Arc::new(ReqwestLlmHttpClient::new()),
            Arc::new(AccountTokens(account)),
        ));
        // The model stays "provider-default": the website picks the model. No credential
        // is configured; the token client attaches the account token to every call.
        let llm = CustomOpenAiProvider::new(
            LlmProviderConfig {
                provider_id: AssistantEngine::LumviseCloud.value().to_string(),
                kind: LlmProviderKind::OpenAiCompatible,
                model: "provider-default".to_string(),
                endpoint: Some(endpoint.clone()),
                credential: None,
                completion_concurrency: None,
                spawn: None,
            },
            Arc::clone(&client),
        )
        .map_err(|error| error.to_string())?;
        let config = OpenRouterSpeechConfig {
            endpoint,
            credential: String::new(),
            model: "lumvise-cloud".into(),
            voice: None,
            timeout_ms: 60_000,
        };
        Ok(Self {
            llm: Arc::new(llm),
            recognizer: Arc::new(OpenRouterSpeechRecognizer::new(
                config.clone(),
                Arc::clone(&client),
            )),
            synthesizer: Arc::new(OpenRouterSpeechSynthesizer::new(config, client)),
            recognizer_installed: false,
            synthesizer_installed: false,
        })
    }

    /// The account's LLM provider, built once so its executor survives registry publishes.
    pub(super) fn llm_provider(&self) -> Arc<dyn LlmProvider> {
        Arc::clone(&self.llm)
    }

    /// Moves each speech slot to the source `settings` ask for; a slot that is
    /// already there is left alone, so applying the same settings twice does nothing.
    /// A switched-off component keeps its source: the enabled flag gates use, so no
    /// local model is loaded just because a hosted component was turned off.
    fn apply(&mut self, app: &AppCore, settings: &AppSettings) -> Result<(), String> {
        let recognition = converge(
            &mut self.recognizer_installed,
            settings.speech_recognition_cloud,
            |hosted| {
                if hosted {
                    app.replace_voice2text_service(Arc::clone(&self.recognizer))
                        .map_err(|error| error.to_string())
                } else {
                    restore_local(app, ManagedModelKind::SpeechToText)
                }
            },
        );
        let synthesis = converge(
            &mut self.synthesizer_installed,
            settings.speech_synthesis_cloud,
            |hosted| {
                if hosted {
                    app.replace_text2voice_service(Arc::clone(&self.synthesizer))
                        .map_err(|error| error.to_string())
                } else {
                    restore_local(app, ManagedModelKind::TextToSpeech)
                }
            },
        );
        recognition.and(synthesis)
    }
}

/// Runs `switch(want)` only when the slot is not already in the wanted state.
fn converge(
    installed: &mut bool,
    want: bool,
    switch: impl FnOnce(bool) -> Result<(), String>,
) -> Result<(), String> {
    if *installed == want {
        return Ok(());
    }
    let result = switch(want);
    // A failed return to local clears the slot, so the hosted adapter is gone either way.
    *installed = want && result.is_ok();
    result
}

/// Puts a component back on its locally selected managed model. Without a selection,
/// or when it cannot be activated, the slot is cleared so the hosted adapter never
/// stays installed by accident.
fn restore_local(app: &AppCore, kind: ManagedModelKind) -> Result<(), String> {
    let selected = app.managed_models().and_then(|manager| {
        let model = manager
            .snapshot()
            .models
            .into_iter()
            .find(|model| model.kind == kind && model.active)?;
        Some((manager, model.model_id))
    });
    let Some((manager, model_id)) = selected else {
        return clear_slot(app, kind);
    };
    let activated = manager
        .select(&model_id)
        .map_err(|error| error.to_string())
        .and_then(|status| match status.state {
            ManagedModelState::Ready => Ok(()),
            _ => Err(format!("local model `{model_id}` is not ready")),
        });
    if activated.is_err() {
        clear_slot(app, kind)?;
    }
    activated
}

fn clear_slot(app: &AppCore, kind: ManagedModelKind) -> Result<(), String> {
    match kind {
        ManagedModelKind::SpeechToText => app.clear_voice2text_service(),
        ManagedModelKind::TextToSpeech => app.clear_text2voice_service(),
        ManagedModelKind::Vector => return Ok(()),
    }
    .map_err(|error| error.to_string())
}

/// Whether a settings patch can change which source serves speech.
pub(super) fn changes_speech_source(patch: &AppSettingsPatch) -> bool {
    matches!(
        patch,
        AppSettingsPatch::SpeechRecognitionCloud(_) | AppSettingsPatch::SpeechSynthesisCloud(_)
    )
}

impl AppCoreDesktopBridge {
    /// Aligns the speech slots with the saved settings, e.g. after the hosted account arrives.
    /// Without an installed account the settings flags change nothing. A failure only
    /// leaves the affected component unavailable, so it is logged and never fails the caller.
    pub(super) fn apply_hosted_ai(&self) {
        let Ok(mut hosted) = self.hosted_ai.lock() else {
            tracing::warn!(
                event = "hosted_ai_apply_failed",
                error = "state lock poisoned"
            );
            return;
        };
        let Some(hosted) = hosted.as_mut() else {
            return;
        };
        let result = self
            .app
            .frontend()
            .app_settings()
            .map_err(|error| error.to_string())
            .and_then(|settings| hosted.apply(&self.app, &settings));
        if let Err(error) = result {
            tracing::warn!(event = "hosted_ai_apply_failed", %error);
        }
    }
}
