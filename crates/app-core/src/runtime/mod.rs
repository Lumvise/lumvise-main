mod compiled_plugins;

use crate::plugin::production::ProductionPluginBootstrap;
use crate::plugin::{SharedPluginVectorizer, VectorEngineIdentity};
use crate::{
    AppCoreHostCapabilityBroker, DesktopBroadcastRecord, PluginEndpoints, PluginProductionConfig,
    Result, ScreenFrameBroadcastRecord, ScreenshotRecord,
};
use lumvise_db_core::{LocalPersistence, RelationalPersistence, SemanticPersistence};
use lumvise_frontend_core::FrontendCore;

use lumvise_neural_core::{LlmProviderCatalog, LlmProviderSync};
use lumvise_neural_core::{
    LlmProviderRegistry, SpeechRecognizer, SpeechSynthesizer, Text2VoiceService, Voice2TextService,
};
use lumvise_plugin_runtime::{PluginRepository, PluginSystem};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Lifetime of a credential minted through [`AppCore::mint_bridge_credential`];
/// matches the runtime coordinator's own grant window.
const BRIDGE_CREDENTIAL_GRANT_TTL_MS: u64 = 60_000;

#[cfg(feature = "assistant-e2e")]
use crate::app::e2e_control::E2eEventJournal;

pub struct AppCore {
    pub(crate) semantic: Arc<dyn SemanticPersistence>,
    pub(crate) semantic_snapshots: Arc<crate::SemanticSnapshotService>,
    pub(crate) relational: Arc<dyn RelationalPersistence>,
    pub(crate) frontend: Arc<Mutex<FrontendCore>>,
    pub(crate) llm_registry: Arc<Mutex<LlmProviderRegistry>>,

    pub(crate) provider_catalog: Arc<Mutex<LlmProviderCatalog>>,
    pub(crate) voice2text_service: Mutex<Option<Arc<dyn SpeechRecognizer>>>,
    pub(crate) text2voice_service: Mutex<Option<Arc<dyn SpeechSynthesizer>>>,
    pub(crate) screenshots: Mutex<Vec<ScreenshotRecord>>,
    pub(crate) desktop_broadcasts: Mutex<Vec<DesktopBroadcastRecord>>,
    pub(crate) screen_frame_broadcasts: Mutex<Vec<ScreenFrameBroadcastRecord>>,
    pub(crate) plugin_vectorizer: SharedPluginVectorizer,
    pub(crate) active_vector_engine: Mutex<Option<VectorEngineIdentity>>,
    plugin_system: Arc<PluginSystem>,
    plugin_repository: Option<Arc<Mutex<PluginRepository>>>,
    recurring_registration_signal: Arc<(Mutex<u64>, Condvar)>,
    initial_plugin_restore: Option<Arc<(Mutex<bool>, Condvar)>>,
    #[cfg(test)]
    plugin_host_capability_broker: Option<Arc<AppCoreHostCapabilityBroker>>,
    pub(crate) scoped_mcp_base_url: Arc<Mutex<Option<String>>>,
    pub(crate) bridge_credentials: Arc<Mutex<Option<Arc<Mutex<HashMap<String, u64>>>>>>,
    pub(crate) frontend_actions: Arc<Mutex<Vec<serde_json::Value>>>,
    pub(crate) project_import_sessions: Mutex<crate::app::project_import::ProjectImportSessions>,
    #[cfg(feature = "assistant-e2e")]
    pub(crate) e2e_event_journal: E2eEventJournal,
    pub(crate) plugin_host_services: Arc<crate::plugin::PluginHostServices>,
    pub(crate) managed_models:
        OnceLock<Arc<lumvise_neural_core::managed_models::ManagedModelManager>>,
    project_execution: crate::ProjectExecutionService,
    pub(crate) project_execution_control:
        crate::project_execution::ProjectExecutionControlTransport,
}

impl AppCore {
    /// Builds App Core from the selected persistence, frontend, and LLM runtime dependencies.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(app.llms().provider_ids().unwrap().is_empty());
    /// ```
    pub fn new(
        semantic: Arc<dyn SemanticPersistence>,
        relational: Arc<dyn RelationalPersistence>,
        frontend: FrontendCore,
        llm_registry: LlmProviderRegistry,
    ) -> Self {
        let vectorizer = Arc::new(Mutex::new(None));
        let services =
            crate::plugin::PluginHostServices::new(frontend, llm_registry, Arc::clone(&semantic));
        let recurring_registration_signal = Arc::new((Mutex::new(0), Condvar::new()));
        let semantic_snapshots =
            Arc::new(crate::SemanticSnapshotService::new(Arc::clone(&semantic)));
        let broker = Arc::new(AppCoreHostCapabilityBroker::with_services(
            Arc::clone(&semantic),
            Arc::clone(&relational),
            vectorizer.clone(),
            Some(Arc::clone(&services)),
            Arc::clone(&semantic_snapshots),
        ));
        let plugin_system = Arc::new(PluginSystem::with_broker_and_lanes(
            Default::default(),
            broker.clone(),
            services.exclusive_lanes(),
        ));
        let semantic_snapshots = broker.semantic_snapshot_service();
        Self::build(
            semantic,
            relational,
            services,
            plugin_system,
            None,
            vectorizer,
            recurring_registration_signal,
            None,
            semantic_snapshots,
            #[cfg(test)]
            Some(broker),
        )
    }

    /// Builds App Core with an injected compiled-plugin lifecycle boundary.
    pub fn new_with_plugin_system(
        semantic: Arc<dyn SemanticPersistence>,
        relational: Arc<dyn RelationalPersistence>,
        frontend: FrontendCore,
        llm_registry: LlmProviderRegistry,
        plugin_system: Arc<PluginSystem>,
    ) -> Self {
        let semantic_snapshots =
            Arc::new(crate::SemanticSnapshotService::new(Arc::clone(&semantic)));
        let services =
            crate::plugin::PluginHostServices::new(frontend, llm_registry, Arc::clone(&semantic));
        let app = Self::build(
            semantic,
            relational,
            services,
            plugin_system,
            None,
            Arc::new(Mutex::new(None)),
            Arc::new((Mutex::new(0), Condvar::new())),
            None,
            semantic_snapshots,
            #[cfg(test)]
            None,
        );
        // The plugin system arrives pre-started, so this construction is the
        // lifecycle event that must publish its background registrations.
        // Anchor recurring cadences at epoch 0 so the first slot is due
        // regardless of the clock later passed to the delivery endpoints.
        let _ = app.plugin_endpoints().sync_background_catalog(0);
        app
    }

    /// Builds production App Core with persistent trust, grants, and plugin registry.
    ///
    /// Enabled packages are restored and started on a background task while this
    /// function returns.
    pub fn new_production(
        semantic: Arc<dyn SemanticPersistence>,
        relational: Arc<dyn RelationalPersistence>,
        frontend: FrontendCore,
        llm_registry: LlmProviderRegistry,
        config: PluginProductionConfig,
    ) -> Result<Self> {
        let vectorizer = Arc::new(Mutex::new(None));
        let services =
            crate::plugin::PluginHostServices::new(frontend, llm_registry, Arc::clone(&semantic));
        let recurring_registration_signal = Arc::new((Mutex::new(0), Condvar::new()));
        let semantic_snapshots =
            Arc::new(crate::SemanticSnapshotService::new(Arc::clone(&semantic)));
        let bootstrap = ProductionPluginBootstrap::open(
            config,
            Arc::clone(&semantic),
            Arc::clone(&relational),
            vectorizer.clone(),
            Arc::clone(&services),
            Arc::clone(&semantic_snapshots),
            Arc::clone(&recurring_registration_signal),
        )?;
        Ok(Self::build(
            semantic,
            relational,
            services,
            bootstrap.system,
            Some(bootstrap.repository),
            vectorizer,
            recurring_registration_signal,
            Some(bootstrap.initial_restore),
            semantic_snapshots,
            #[cfg(test)]
            None,
        ))
    }

    /// Replaces the dispatch registry and its UI catalog as one application
    /// transition. The catalog is published only after the registry is ready,
    /// so the UI never receives a model before it can be dispatched.
    pub(crate) fn replace_synchronized_providers(&self, sync: LlmProviderSync) -> Result<()> {
        let mut registry = self
            .llm_registry
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("llm_registry"))?;
        let mut catalog = self
            .provider_catalog
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("provider_catalog"))?;
        *registry = sync.registry;
        *catalog = sync.catalog;
        Ok(())
    }
    fn build(
        semantic: Arc<dyn SemanticPersistence>,
        relational: Arc<dyn RelationalPersistence>,
        plugin_host_services: Arc<crate::plugin::PluginHostServices>,
        plugin_system: Arc<PluginSystem>,
        plugin_repository: Option<Arc<Mutex<PluginRepository>>>,
        plugin_vectorizer: SharedPluginVectorizer,
        recurring_registration_signal: Arc<(Mutex<u64>, Condvar)>,
        initial_plugin_restore: Option<Arc<(Mutex<bool>, Condvar)>>,
        semantic_snapshots: Arc<crate::SemanticSnapshotService>,
        #[cfg(test)] plugin_host_capability_broker: Option<Arc<AppCoreHostCapabilityBroker>>,
    ) -> Self {
        let frontend = plugin_host_services.frontend();
        let project_execution = crate::ProjectExecutionService::with_local_artifacts(
            Arc::clone(&frontend),
            plugin_host_services.llm_executors(),
            plugin_host_services.activity(),
        );
        plugin_host_services.install_project_execution(project_execution.clone());
        Self {
            semantic,
            relational,
            frontend,
            llm_registry: plugin_host_services.llms(),
            semantic_snapshots,
            voice2text_service: Mutex::new(None),

            provider_catalog: Arc::new(Mutex::new(LlmProviderCatalog::default())),
            text2voice_service: Mutex::new(None),
            screenshots: Mutex::new(Vec::new()),
            desktop_broadcasts: Mutex::new(Vec::new()),
            screen_frame_broadcasts: Mutex::new(Vec::new()),
            plugin_vectorizer,
            active_vector_engine: Mutex::new(None),
            plugin_system,
            plugin_repository,
            recurring_registration_signal,
            initial_plugin_restore,
            #[cfg(test)]
            plugin_host_capability_broker,
            scoped_mcp_base_url: plugin_host_services.scoped_mcp_base_url(),
            bridge_credentials: Arc::new(Mutex::new(None)),
            frontend_actions: plugin_host_services.frontend_actions(),
            project_import_sessions: Mutex::new(Default::default()),
            #[cfg(feature = "assistant-e2e")]
            e2e_event_journal: E2eEventJournal::default(),
            plugin_host_services,
            managed_models: OnceLock::new(),
            project_execution,
            project_execution_control: Default::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn default_plugin_host_capability_broker(
        &self,
    ) -> Option<&AppCoreHostCapabilityBroker> {
        self.plugin_host_capability_broker.as_deref()
    }

    /// Builds App Core with live STT/TTS services for assistant voice turns.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let app = AppCore::new_with_speech_services(db, frontend, llms, stt, tts);
    /// ```
    pub fn new_with_speech_services(
        semantic: Arc<dyn SemanticPersistence>,
        relational: Arc<dyn RelationalPersistence>,
        frontend: FrontendCore,
        llm_registry: LlmProviderRegistry,
        voice2text_service: Voice2TextService,
        text2voice_service: Text2VoiceService,
    ) -> Self {
        let app = Self::new(semantic, relational, frontend, llm_registry);
        let voice2text_service: Arc<dyn SpeechRecognizer> = Arc::new(voice2text_service);
        let text2voice_service: Arc<dyn SpeechSynthesizer> = Arc::new(text2voice_service);
        app.with_speech_recognizer(voice2text_service)
            .with_speech_synthesizer(text2voice_service)
    }

    /// Installs a live speech-to-text service for voice transcription endpoints.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let app = AppCore::in_memory()?.with_voice2text_service(stt);
    /// ```
    pub fn with_voice2text_service(self, voice2text_service: Voice2TextService) -> Self {
        self.with_speech_recognizer(Arc::new(voice2text_service))
    }

    /// Installs a live text-to-voice service for assistant playback endpoints.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let app = AppCore::in_memory()?.with_text2voice_service(tts);
    /// ```
    pub fn with_text2voice_service(self, text2voice_service: Text2VoiceService) -> Self {
        self.with_speech_synthesizer(Arc::new(text2voice_service))
    }

    /// Installs the one selected speech recognizer without exposing its local
    /// or centralized implementation to App Core consumers.
    pub(crate) fn with_speech_recognizer(self, service: Arc<dyn SpeechRecognizer>) -> Self {
        let _ = self.replace_voice2text_service(service);
        self
    }

    /// Installs the one selected speech synthesizer without exposing its local
    /// or centralized implementation to App Core consumers.
    pub(crate) fn with_speech_synthesizer(self, service: Arc<dyn SpeechSynthesizer>) -> Self {
        let _ = self.replace_text2voice_service(service);
        self
    }

    /// Replaces the active speech recognizer at runtime. Applies to newly
    /// created voice sessions; a transcription already in flight keeps
    /// running on the model instance it started with.
    pub fn replace_voice2text_service(&self, service: Arc<dyn SpeechRecognizer>) -> Result<()> {
        self.plugin_host_services
            .install_speech_recognizer(Arc::clone(&service));
        *self
            .voice2text_service
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("voice2text_service"))? =
            Some(service);
        Ok(())
    }

    /// Replaces the active speech synthesizer at runtime. Applies to newly
    /// created voice sessions; playback already in flight keeps running on
    /// the model instance it started with.
    pub fn replace_text2voice_service(&self, service: Arc<dyn SpeechSynthesizer>) -> Result<()> {
        self.plugin_host_services
            .install_speech_synthesizer(Arc::clone(&service));
        *self
            .text2voice_service
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("text2voice_service"))? =
            Some(service);
        Ok(())
    }

    /// Returns the currently installed speech recognizer, if any.
    pub fn voice2text_service(&self) -> Option<Arc<dyn SpeechRecognizer>> {
        self.voice2text_service
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    /// Returns the currently installed speech synthesizer, if any.
    pub fn text2voice_service(&self) -> Option<Arc<dyn SpeechSynthesizer>> {
        self.text2voice_service
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    /// Installs the managed-model lifecycle manager and restores every
    /// persisted active selection into the live vector/speech runtime.
    ///
    /// Call once, immediately after wrapping a freshly built App Core in
    /// `Arc`; a second call is a harmless no-op.
    pub fn install_managed_models(self: &Arc<Self>) -> Result<()> {
        if self.managed_models.get().is_some() {
            return Ok(());
        }
        let adapter = Arc::new(crate::app::managed_models::AppCoreModelRuntimeAdapter::new(
            Arc::downgrade(self),
        ));
        let manager =
            lumvise_neural_core::managed_models::ManagedModelManager::production(adapter)?;
        manager.restore_active_selections().map_err(|message| {
            crate::AppCoreError::invalid_value(message, "managed models to restore cleanly")
        })?;
        let _ = self.managed_models.set(Arc::new(manager));
        Ok(())
    }

    /// Returns the managed-model lifecycle manager, once installed.
    pub fn managed_models(
        &self,
    ) -> Option<Arc<lumvise_neural_core::managed_models::ManagedModelManager>> {
        self.managed_models.get().cloned()
    }
    /// Queues one frontend action for a connected desktop renderer.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// app.enqueue_frontend_action(serde_json::json!({ "action": "frontend.open_dashboard" })).unwrap();
    /// assert_eq!(app.drain_frontend_actions().unwrap().len(), 1);
    /// ```
    pub fn enqueue_frontend_action(&self, action: serde_json::Value) -> Result<()> {
        self.frontend_actions
            .lock()
            .map_err(|_| crate::AppCoreError::unsupported("frontend_actions", "usable queue"))?
            .push(action);
        Ok(())
    }

    /// Drains queued frontend actions for the connected desktop renderer.
    pub fn drain_frontend_actions(&self) -> Result<Vec<serde_json::Value>> {
        let mut actions = self
            .frontend_actions
            .lock()
            .map_err(|_| crate::AppCoreError::unsupported("frontend_actions", "usable queue"))?;
        Ok(actions.drain(..).collect())
    }

    /// Drains one window's actions without stealing another surface's work.
    /// Example: `app.drain_window_actions("lumvise-workspace")`.
    pub fn drain_window_actions(&self, window_label: &str) -> Result<Vec<serde_json::Value>> {
        let mut actions = self
            .frontend_actions
            .lock()
            .map_err(|_| crate::AppCoreError::unsupported("frontend_actions", "usable queue"))?;
        let (owned, pending) = actions.drain(..).partition(|action| {
            action["payload"]["target_window"]
                .as_str()
                .unwrap_or("lumvise-frontend")
                == window_label
        });
        *actions = pending;
        Ok(owned)
    }

    /// Opens deterministic in-memory App Core state for tests and bootstrapping.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert_eq!(app.frontend().canvas().unwrap().revision, 0);
    /// ```
    pub fn in_memory() -> Result<Self> {
        Self::in_memory_with_llm_registry(LlmProviderRegistry::empty())
    }

    /// Builds the test-only local persistence composition with an explicit LLM registry.
    pub fn in_memory_with_llm_registry(llm_registry: LlmProviderRegistry) -> Result<Self> {
        let (semantic, relational) = Self::in_memory_persistence()?;
        Ok(Self::new(
            semantic,
            relational,
            FrontendCore::default(),
            llm_registry,
        ))
    }

    /// Builds the test-only local persistence composition with an injected plugin system.
    pub fn in_memory_with_plugin_system(plugin_system: Arc<PluginSystem>) -> Result<Self> {
        let (semantic, relational) = Self::in_memory_persistence()?;
        Ok(Self::new_with_plugin_system(
            semantic,
            relational,
            FrontendCore::default(),
            LlmProviderRegistry::empty(),
            plugin_system,
        ))
    }

    fn in_memory_persistence()
    -> Result<(Arc<dyn SemanticPersistence>, Arc<dyn RelationalPersistence>)> {
        let persistence = Arc::new(LocalPersistence::in_memory()?);
        let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
        let relational: Arc<dyn RelationalPersistence> = persistence;
        Ok((semantic, relational))
    }

    /// Starts a complete PZ snapshot for the active project.
    ///
    /// # Example
    ///
    /// ```no_run
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let operation = app.create_semantic_snapshot().unwrap();
    /// assert!(!operation.operation_id.is_empty());
    /// ```
    pub fn create_semantic_snapshot(&self) -> Result<crate::SemanticSnapshotOperation> {
        self.semantic_snapshots.create()
    }

    /// Reads one semantic PZ snapshot operation status/result.
    ///
    /// # Example
    ///
    /// ```no_run
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let created = app.create_semantic_snapshot().unwrap();
    /// let status = app.semantic_snapshot_status(&created.operation_id).unwrap();
    /// assert_eq!(status.operation_id, created.operation_id);
    /// ```
    pub fn semantic_snapshot_status(
        &self,
        operation_id: &str,
    ) -> Result<crate::SemanticSnapshotOperation> {
        self.semantic_snapshots.status(operation_id)
    }

    /// Requests cancellation of one semantic PZ snapshot operation.
    ///
    /// # Example
    ///
    /// ```no_run
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let created = app.create_semantic_snapshot().unwrap();
    /// let cancelled = app.cancel_semantic_snapshot(&created.operation_id).unwrap();
    /// assert_eq!(cancelled.operation_id, created.operation_id);
    /// ```
    pub fn cancel_semantic_snapshot(
        &self,
        operation_id: &str,
    ) -> Result<crate::SemanticSnapshotOperation> {
        self.semantic_snapshots.cancel(operation_id)
    }

    /// Returns the central plugin-facing endpoint bundle.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let _endpoints = app.plugin_endpoints();
    /// ```

    pub fn plugin_endpoints(&self) -> PluginEndpoints<'_> {
        PluginEndpoints::new(self)
    }

    pub fn host_capability_broker(&self) -> AppCoreHostCapabilityBroker {
        AppCoreHostCapabilityBroker::with_services(
            Arc::clone(&self.semantic),
            Arc::clone(&self.relational),
            self.plugin_vectorizer.clone(),
            Some(Arc::clone(&self.plugin_host_services)),
            Arc::clone(&self.semantic_snapshots),
        )
    }

    /// Returns Frontend Core interaction endpoints.
    ///
    /// # Example
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert_eq!(app.frontend().canvas().unwrap().canvas_id, "main");
    /// ```
    pub fn frontend(&self) -> crate::FrontendInteractionEndpoints<'_> {
        crate::FrontendInteractionEndpoints::new(self)
    }

    /// Returns application modality endpoints.
    ///
    /// # Example
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(app.modalities().latest_screenshot().unwrap().is_none());
    /// ```
    pub fn modalities(&self) -> crate::ModalityEndpoints<'_> {
        crate::ModalityEndpoints::new(self)
    }

    /// Returns configured LLM Provider endpoints.
    ///
    /// # Example
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(app.llms().provider_ids().unwrap().is_empty());
    /// ```
    pub fn llms(&self) -> crate::LlmEndpoints<'_> {
        crate::LlmEndpoints::new(self)
    }

    /// Returns Plugin-neutral persistence endpoints.
    ///
    /// # Example
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(app.database().setting("app", "missing").unwrap().is_none());
    /// ```
    pub fn database(&self) -> crate::DatabaseEndpoints<'_> {
        crate::DatabaseEndpoints::new(self)
    }

    /// Returns the compiled-plugin installation and process lifecycle boundary.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(!app.plugin_system().is_installed("example").unwrap());
    /// ```
    pub fn plugin_system(&self) -> &PluginSystem {
        &self.plugin_system
    }

    /// Returns a shared handle for threads that outlive one call frame.
    pub(crate) fn plugin_system_handle(&self) -> Arc<PluginSystem> {
        Arc::clone(&self.plugin_system)
    }

    pub(crate) fn cataloged_production_plugin_ids(&self) -> Result<Vec<String>> {
        let mut plugin_ids = self.plugin_system.cataloged_plugin_ids()?;
        if let Some(repository) = &self.plugin_repository {
            plugin_ids.extend(
                repository
                    .lock()
                    .map_err(|_| crate::AppCoreError::poisoned_mutex("plugin_repository"))?
                    .registry()
                    .records
                    .iter()
                    .map(|record| record.plugin_id.clone()),
            );
        }
        plugin_ids.sort();
        plugin_ids.dedup();
        Ok(plugin_ids)
    }

    pub(crate) fn enabled_compiled_plugin_ids(&self) -> Result<Vec<String>> {
        let Some(repository) = &self.plugin_repository else {
            return Ok(self.plugin_system.cataloged_plugin_ids()?);
        };
        let mut plugin_ids = repository
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("plugin_repository"))?
            .registry()
            .records
            .iter()
            .filter(|record| record.enabled)
            .map(|record| record.plugin_id.clone())
            .collect::<Vec<_>>();
        plugin_ids.sort();
        plugin_ids.dedup();
        Ok(plugin_ids)
    }

    pub(crate) fn background_registration_generation(&self) -> Result<u64> {
        let (generation, _) = self.recurring_registration_signal.as_ref();
        generation
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("recurring_registration_signal"))
            .map(|value| *value)
    }

    pub(crate) fn wait_for_background_registration_change(
        &self,
        expected_generation: u64,
        timeout: Option<std::time::Duration>,
    ) -> Result<bool> {
        let (generation, notifier) = self.recurring_registration_signal.as_ref();
        let mut seen = generation
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("recurring_registration_signal"))?;
        if *seen != expected_generation {
            return Ok(true);
        }
        match timeout {
            Some(timeout) => {
                let (updated, timed_out) = notifier.wait_timeout(seen, timeout).map_err(|_| {
                    crate::AppCoreError::poisoned_mutex("recurring_registration_signal")
                })?;
                seen = updated;
                if timed_out.timed_out() {
                    return Ok(false);
                }
                Ok(*seen != expected_generation)
            }
            None => {
                let updated = notifier.wait(seen).map_err(|_| {
                    crate::AppCoreError::poisoned_mutex("recurring_registration_signal")
                })?;
                seen = updated;
                Ok(*seen != expected_generation)
            }
        }
    }

    pub(crate) fn wait_for_initial_plugin_restore(
        &self,
        timeout: std::time::Duration,
    ) -> Result<bool> {
        let Some(initial_restore) = &self.initial_plugin_restore else {
            return Ok(true);
        };
        let (complete, notifier) = initial_restore.as_ref();
        let complete = complete
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("initial_plugin_restore"))?;
        if *complete {
            return Ok(true);
        }
        let (complete, _) = notifier
            .wait_timeout_while(complete, timeout, |complete| !*complete)
            .map_err(|_| crate::AppCoreError::poisoned_mutex("initial_plugin_restore"))?;
        Ok(*complete)
    }

    pub(crate) fn notify_background_registration_change(&self) -> Result<()> {
        let (generation, notifier) = self.recurring_registration_signal.as_ref();
        let mut generation = generation
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("recurring_registration_signal"))?;
        *generation = generation.saturating_add(1);
        notifier.notify_all();
        Ok(())
    }

    pub(crate) fn production_plugin_repository(&self) -> Result<MutexGuard<'_, PluginRepository>> {
        self.plugin_repository
            .as_ref()
            .ok_or_else(|| {
                crate::AppCoreError::unsupported(
                    "compiled plugin lifecycle",
                    "AppCore constructed with PluginProductionConfig",
                )
            })?
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("plugin_repository"))
    }

    /// Returns AppCore's transport-neutral project execution boundary.
    pub fn project_execution(&self) -> &crate::ProjectExecutionService {
        &self.project_execution
    }

    pub(crate) fn set_scoped_mcp_base_url(&self, base_url: Option<String>) -> Result<()> {
        *self
            .scoped_mcp_base_url
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("scoped_mcp_base_url"))? =
            normalized_base_url(base_url);
        Ok(())
    }

    pub(crate) fn clear_scoped_mcp_base_url_if(&self, base_url: &str) -> Result<()> {
        let mut current = self
            .scoped_mcp_base_url
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("scoped_mcp_base_url"))?;
        if current.as_deref() == Some(base_url) {
            *current = None;
        }
        Ok(())
    }

    pub(crate) fn install_bridge_credential_store(
        &self,
        store: Arc<Mutex<HashMap<String, u64>>>,
    ) -> Result<()> {
        *self
            .bridge_credentials
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("bridge_credentials"))? = Some(store);
        Ok(())
    }

    pub(crate) fn set_bridge_credential(
        &self,
        credential: Option<String>,
        expires_unix_ms: u64,
    ) -> Result<()> {
        let store = self
            .bridge_credentials
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("bridge_credentials"))?
            .clone()
            .ok_or_else(|| {
                crate::AppCoreError::invalid_value(
                    "bridge credential store",
                    "installed before readiness",
                )
            })?;
        let mut store = store
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("bridge_credentials"))?;
        store.clear();
        if let Some(credential) = credential {
            store.insert(credential, expires_unix_ms);
        }
        Ok(())
    }

    /// Issues an additional short-lived bridge credential without disturbing
    /// the ones already granted. The store is shared with the runtime
    /// coordinator, which prunes expired entries on every match.
    pub(crate) fn mint_bridge_credential(&self) -> Result<(String, u64)> {
        let store = self
            .bridge_credentials
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("bridge_credentials"))?
            .clone()
            .ok_or_else(|| {
                crate::AppCoreError::invalid_value(
                    "bridge credential store",
                    "installed before readiness",
                )
            })?;
        let mut credentials = store
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("bridge_credentials"))?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(u64::MAX);
        credentials.retain(|_, expires| *expires >= now);
        let mut bytes = [0_u8; 24];
        let _ = getrandom::fill(&mut bytes);
        let credential = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let expires_unix_ms = now.saturating_add(BRIDGE_CREDENTIAL_GRANT_TTL_MS);
        credentials.insert(credential.clone(), expires_unix_ms);
        Ok((credential, expires_unix_ms))
    }

    pub(crate) fn bridge_credential_matches(&self, credential: &str) -> bool {
        let Ok(store) = self.bridge_credentials.lock() else {
            return false;
        };
        let Some(store) = store.as_ref() else {
            return false;
        };
        let Ok(mut credentials) = store.lock() else {
            return false;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(u64::MAX);
        credentials.retain(|_, expires| *expires >= now);
        credentials
            .get(credential)
            .is_some_and(|expires| *expires >= now)
    }
}

fn normalized_base_url(base_url: Option<String>) -> Option<String> {
    base_url
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::Path,
        sync::{Arc, LazyLock, Mutex},
    };

    static MANAGED_MODEL_STATE_ROOT_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    struct StateRootGuard(Option<std::ffi::OsString>);

    impl StateRootGuard {
        fn set(path: &Path) -> Self {
            let previous = std::env::var_os("LUMVISE_STATE_ROOT");
            unsafe {
                std::env::set_var("LUMVISE_STATE_ROOT", path);
            }
            Self(previous)
        }
    }

    impl Drop for StateRootGuard {
        fn drop(&mut self) {
            unsafe {
                match self.0.take() {
                    Some(previous) => std::env::set_var("LUMVISE_STATE_ROOT", previous),
                    None => std::env::remove_var("LUMVISE_STATE_ROOT"),
                }
            }
        }
    }

    #[test]
    fn install_managed_models_keeps_app_core_alive_after_restore_failure() {
        let _state_root_lock = MANAGED_MODEL_STATE_ROOT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = tempfile::tempdir().unwrap();
        let models_root = root.path().join("models");
        let published = models_root.join("vector.all-minilm-l6-v2");
        std::fs::create_dir_all(&published).unwrap();
        for relative in [
            "config.json",
            "special_tokens_map.json",
            "tokenizer.json",
            "tokenizer_config.json",
            "model.onnx",
        ] {
            std::fs::write(published.join(relative), b"fixture").unwrap();
        }
        std::fs::write(
            models_root.join(".active-selections.json"),
            br#"{"vector":"vector.all-minilm-l6-v2"}"#,
        )
        .unwrap();
        let _state_root = StateRootGuard::set(root.path());

        let app = Arc::new(AppCore::in_memory().unwrap());
        app.install_managed_models()
            .expect("recoverable model restore must not abort App Core startup");

        let manager = app
            .managed_models()
            .expect("managed model manager must be owned by App Core");
        let status = manager.status_for("vector.all-minilm-l6-v2").unwrap();
        assert_eq!(
            status.state,
            lumvise_neural_core::managed_models::ManagedModelState::Failed
        );
        assert!(!status.active);
    }

    #[test]
    fn initial_plugin_restore_wait_reports_timeout_then_completion() {
        let mut app = AppCore::in_memory().unwrap();
        let initial_restore = Arc::new((Mutex::new(false), Condvar::new()));
        app.initial_plugin_restore = Some(Arc::clone(&initial_restore));
        assert!(
            !app.wait_for_initial_plugin_restore(std::time::Duration::from_millis(1))
                .unwrap()
        );

        std::thread::spawn(move || {
            let (complete, notifier) = initial_restore.as_ref();
            *complete.lock().unwrap() = true;
            notifier.notify_all();
        });
        assert!(
            app.wait_for_initial_plugin_restore(std::time::Duration::from_secs(1))
                .unwrap()
        );
    }
}
