use crate::{AppCore, AppCoreError, Result};
use lumvise_neural_core::llm_providers::{
    LlmExecutionControl, LlmProviderCapabilities, LlmRequest, LlmResponse, LlmStreamEvent,
};
use lumvise_neural_core::process::StreamControl;
use lumvise_resource_routing::InvocationControl;

pub struct LlmEndpoints<'app> {
    app: &'app AppCore,
}

impl<'app> LlmEndpoints<'app> {
    pub(crate) fn new(app: &'app AppCore) -> Self {
        Self { app }
    }

    /// Lists configured local or remote LLM provider ids.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(app.llms().provider_ids().unwrap().is_empty());
    /// ```
    pub fn provider_ids(&self) -> Result<Vec<String>> {
        Ok(self.lock_registry()?.provider_ids())
    }

    /// Returns modality and streaming capabilities for one configured provider.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(app.llms().capabilities("missing").is_err());
    /// ```
    pub fn capabilities(&self, provider_id: &str) -> Result<LlmProviderCapabilities> {
        Ok(self.lock_registry()?.capabilities(provider_id)?)
    }

    /// Completes one LLM request through Neural Core.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let request = lumvise_neural_core::llm_providers::LlmRequest {
    ///     messages: vec![], stream: false, provider_id: None, model: None,
    ///     conversation_id: None, provider_session_id: None,
    ///     mcp_servers: vec![], modality_inputs: vec![], options: Default::default(),
    /// };
    /// assert!(app.llms().complete("missing", &request).is_err());
    /// ```
    pub fn complete(&self, provider_id: &str, request: &LlmRequest) -> Result<LlmResponse> {
        // T6.4: route through the shared per-provider executor pool so HTTP
        // providers (Cerebras/OpenRouter/z.ai) run concurrent completions and a
        // slow provider cannot serialize against unrelated providers. The
        // registry mutex is held only for the brief provider lookup, not the
        // completion itself.
        let ticket = self
            .app
            .plugin_host_services
            .activity()
            .track_llm("app", provider_id);
        let result = self.app.plugin_host_services.llm_executors().complete(
            provider_id,
            request.clone(),
            ticket.control(LlmExecutionControl::new(InvocationControl::sixty_seconds())),
        );
        ticket.finish(&result);
        Ok(result?)
    }

    /// Streams one LLM request through Neural Core.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let request = lumvise_neural_core::llm_providers::LlmRequest {
    ///     messages: vec![], stream: true, provider_id: None, model: None,
    ///     conversation_id: None, provider_session_id: None,
    ///     mcp_servers: vec![], modality_inputs: vec![], options: Default::default(),
    /// };
    /// assert!(app.llms().stream("missing", &request, lumvise_neural_core::process::StreamControl::unbounded()).is_err());
    /// ```
    pub fn stream(
        &self,
        provider_id: &str,
        request: &LlmRequest,
        control: StreamControl,
    ) -> Result<Vec<LlmStreamEvent>> {
        let mut events = Vec::new();
        self.stream_with_events(provider_id, request, control, &mut |event| {
            events.push(event);
            Ok(())
        })?;
        Ok(events)
    }

    pub fn stream_with_events(
        &self,
        provider_id: &str,
        request: &LlmRequest,
        control: StreamControl,
        on_event: &mut dyn FnMut(LlmStreamEvent) -> Result<()>,
    ) -> Result<()> {
        let provider = self.stream_provider(provider_id, request)?;
        let ticket = self
            .app
            .plugin_host_services
            .activity()
            .track_llm("app", provider_id);
        let mut callback_error = None;
        ticket.started();
        let result =
            provider.stream_with_events(request, control, &mut |event| match on_event(event) {
                Ok(()) => Ok(()),
                Err(error) => {
                    callback_error = Some(error);
                    Err(stream_callback_error(provider_id))
                }
            });
        ticket.finish_stream(&result);
        if let Some(error) = callback_error {
            return Err(error);
        }
        Ok(result?)
    }

    fn stream_provider(
        &self,
        provider_id: &str,
        request: &LlmRequest,
    ) -> Result<std::sync::Arc<dyn lumvise_neural_core::llm_providers::contract::LlmProvider>> {
        // A live stream must not hold the registry lock needed to spawn the Assistant.
        let registry = self.lock_registry()?;
        registry.negotiate(provider_id, request)?;
        Ok(registry.provider_handle(provider_id)?)
    }

    fn lock_registry(
        &self,
    ) -> Result<std::sync::MutexGuard<'app, lumvise_neural_core::LlmProviderRegistry>> {
        self.app
            .llm_registry
            .lock()
            .map_err(|_| AppCoreError::poisoned_mutex("llm_registry"))
    }
}

fn stream_callback_error(provider_id: &str) -> lumvise_neural_core::NeuralError {
    lumvise_neural_core::NeuralError::ProviderFailed {
        provider_id: provider_id.to_string(),
        message: "app-core LLM stream callback failed".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppCore;
    use lumvise_neural_core::LlmProviderRegistry;
    use lumvise_neural_core::llm_providers::contract::{LlmProvider, LlmStreamEventSink};
    use lumvise_neural_core::llm_providers::{
        LlmCapabilitySupport, LlmMessage, LlmProviderCapabilities,
    };
    use lumvise_neural_core::process::StreamControl;
    use std::sync::mpsc;
    use std::sync::{Arc, Condvar, Mutex};

    /// T6.4: two desktop-path completions on a concurrent provider must overlap
    /// (the endpoint routes through the per-provider executor pool, not a
    /// registry-wide lock).
    struct GatedProvider {
        entered: mpsc::SyncSender<()>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }

    impl LlmProvider for GatedProvider {
        fn provider_id(&self) -> &str {
            "concurrent-http"
        }
        fn capabilities(&self) -> LlmProviderCapabilities {
            unsupported_caps("concurrent-http")
        }
        fn complete(&self, _request: &LlmRequest) -> lumvise_neural_core::Result<LlmResponse> {
            let _ = self.entered.send(());
            let (released, changed) = &*self.release;
            let mut guard = released.lock().unwrap();
            while !*guard {
                guard = changed.wait(guard).unwrap();
            }
            Ok(LlmResponse {
                provider_id: "concurrent-http".to_string(),
                model: String::new(),
                content: "ok".to_string(),
                metadata: serde_json::json!({}),
            })
        }
        fn stream_with_events(
            &self,
            _request: &LlmRequest,
            _control: StreamControl,
            _on_event: &mut LlmStreamEventSink<'_>,
        ) -> lumvise_neural_core::Result<()> {
            Ok(())
        }
    }

    fn unsupported_caps(id: &str) -> LlmProviderCapabilities {
        LlmProviderCapabilities {
            provider_id: id.to_string(),
            final_text_output: LlmCapabilitySupport::Supported,
            streamed_text_output: LlmCapabilitySupport::Unsupported,
            image_snapshot_input: LlmCapabilitySupport::Unsupported,
            live_audio_input: LlmCapabilitySupport::Unsupported,
            screen_frame_broadcast_input: LlmCapabilitySupport::Unsupported,
            native_audio_output: LlmCapabilitySupport::Unsupported,
        }
    }

    fn request() -> LlmRequest {
        LlmRequest {
            options: Default::default(),
            messages: vec![LlmMessage {
                role: "user".into(),
                content: "hi".into(),
            }],
            stream: false,
            provider_id: None,
            model: None,
            conversation_id: None,
            provider_session_id: None,
            mcp_servers: Vec::new(),
            modality_inputs: Vec::new(),
        }
    }

    #[test]
    fn endpoint_completions_run_in_parallel_on_a_concurrent_provider() {
        let (entered_tx, entered_rx) = mpsc::sync_channel(2);
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let registry =
            LlmProviderRegistry::from_provider_instances(vec![Box::new(GatedProvider {
                entered: entered_tx,
                release: Arc::clone(&release),
            })])
            .unwrap()
            .with_completion_concurrency("concurrent-http", 2);
        let app = AppCore::in_memory_with_llm_registry(registry).unwrap();
        let request = request();

        let (first_result, second_result) = std::thread::scope(|scope| {
            let first = scope.spawn(|| app.llms().complete("concurrent-http", &request));
            let second = scope.spawn(|| app.llms().complete("concurrent-http", &request));
            entered_rx
                .recv_timeout(std::time::Duration::from_millis(500))
                .expect("first completion should enter");
            entered_rx
                .recv_timeout(std::time::Duration::from_millis(500))
                .expect("second completion should enter while first is still running");
            {
                let (released, changed) = &*release;
                *released.lock().unwrap() = true;
                changed.notify_all();
            }
            (first.join().unwrap(), second.join().unwrap())
        });

        first_result.unwrap();
        second_result.unwrap();
    }
}

#[cfg(test)]
mod activity_tests;
