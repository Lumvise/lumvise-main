use super::provider_startup::assistant_provider_catalog;
use crate::PluginInvocationRequest;
use crate::{AppCore, AppCoreError, OwnerLease, RuntimeConnection, ScopedMcpHttpServer};
use lumvise_db_core::{
    CompactSemanticGraphProjection, SemanticGraphProjectionRequest, SemanticOperation,
    SemanticResult,
};
use lumvise_frontend_core::{
    AppSettingsPatch, AssistantProviderCatalog, DesktopAppBridgeRequest, DesktopAppBridgeResponse,
    DesktopPluginViewAsset, DesktopSemanticGraphBridge, DesktopSemanticGraphRequest,
    DesktopSettingsBridge, DesktopSpeechStreamEvent, DesktopSpeechStreamEventSink,
    DesktopVoiceBridge, DesktopVoicePlaybackEvent, DesktopVoicePlaybackEventSink,
    DesktopWhiteboardBridge, VoicePlaybackStatus, WorkArea,
};
use lumvise_neural_core::NeuralError;
use lumvise_neural_core::text2voice::{Text2VoiceRequest, Text2VoiceStreamEvent};
use lumvise_neural_core::voice2text::Voice2TextRequest;
use lumvise_resource_routing::InvocationControl;
use serde_json::json;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

mod storage_revisions;

#[derive(Clone)]
pub struct AppCoreDesktopBridge {
    app: Arc<AppCore>,
    runtime_connection: Option<RuntimeConnection>,
    _scoped_plugin_mcp_server: Option<Arc<ScopedMcpHttpServer>>,
    _runtime_owner: Arc<Mutex<Option<OwnerLease>>>,
}
impl AppCoreDesktopBridge {
    /// Bridges desktop requests into App Core and owns the ready Runtime generation.
    pub fn new(app: Arc<AppCore>, owner: OwnerLease) -> Result<Self, String> {
        Self::new_with_owner_cell(app, Arc::new(Mutex::new(Some(owner))))
    }

    pub(crate) fn new_with_owner_cell(
        app: Arc<AppCore>,
        owner_cell: Arc<Mutex<Option<OwnerLease>>>,
    ) -> Result<Self, String> {
        let mut owner_guard = owner_cell
            .lock()
            .map_err(|_| "runtime owner lock poisoned".to_string())?;
        let owner = owner_guard
            .as_mut()
            .ok_or_else(|| "runtime owner missing during bridge setup".to_string())?;
        let (server, connection) = super::runtime_bridge::start_runtime_bridge(&app, owner)?;
        Ok(Self {
            app,
            runtime_connection: Some(connection),
            _scoped_plugin_mcp_server: Some(Arc::new(server)),
            _runtime_owner: Arc::clone(&owner_cell),
        })
    }

    #[cfg(test)]
    pub(crate) fn without_runtime(app: Arc<AppCore>) -> Self {
        Self {
            app,
            runtime_connection: None,
            _scoped_plugin_mcp_server: None,
            _runtime_owner: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns the current generation-bound bridge connection for request pinning.
    pub fn runtime_connection(&self) -> Option<&RuntimeConnection> {
        self.runtime_connection.as_ref()
    }
}

impl std::fmt::Debug for AppCoreDesktopBridge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppCoreDesktopBridge")
            .finish_non_exhaustive()
    }
}

fn normalized_graph_identity(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn resolved_graph_project_root(request: &DesktopSemanticGraphRequest) -> Result<String, String> {
    let provider_id = normalized_graph_identity(request.provider_id.as_deref());
    let project_root = normalized_graph_identity(request.project_root.as_deref());
    match (provider_id, project_root) {
        (Some(provider_id), Some(project_root)) if provider_id != project_root => {
            Err("providerId must equal projectRoot for the app-owned semantic index".to_string())
        }
        (Some(provider_id), _) => Ok(provider_id),
        (_, Some(project_root)) => Ok(project_root),
        (None, None) => {
            Err("semantic graph request requires providerId or projectRoot".to_string())
        }
    }
}

impl DesktopSemanticGraphBridge for AppCoreDesktopBridge {
    fn wait_for_storage_revision(&self, after_revision: i64) -> Result<i64, String> {
        storage_revisions::wait_for_revision(self.app.semantic.as_ref(), after_revision)
    }

    fn list_indexed_semantic_graph_roots(&self) -> Result<Vec<String>, String> {
        match self.app.semantic.execute(
            SemanticOperation::ProjectRoots,
            &InvocationControl::sixty_seconds(),
        ) {
            Ok(SemanticResult::ProjectRoots(roots)) => Ok(roots),
            Ok(result) => Err(format!(
                "project roots returned unexpected semantic result: {result:?}"
            )),
            Err(error) => Err(error.to_string()),
        }
    }

    fn project_indexed_semantic_graph(
        &self,
        request: DesktopSemanticGraphRequest,
    ) -> Result<CompactSemanticGraphProjection, String> {
        let project_root = resolved_graph_project_root(&request)?;
        let operation = SemanticOperation::ProjectRendererGraph(SemanticGraphProjectionRequest {
            project_root,
            target_path: request.target_path,
            granularity: request.granularity,
            recursive: request.recursive,
            include_external: request.include_external,
            include_first_neighbors: request.include_first_neighbors,
        });
        match self
            .app
            .semantic
            .execute(operation, &InvocationControl::sixty_seconds())
        {
            Ok(SemanticResult::RendererGraphProjection(projection)) => {
                projection.into_compact().map_err(|error| error.to_string())
            }
            Ok(result) => Err(format!(
                "semantic graph returned unexpected semantic result: {result:?}"
            )),
            Err(error) => Err(error.to_string()),
        }
    }

    fn read_source_file_bytes(
        &self,
        project_root: String,
        path: String,
    ) -> Result<Vec<u8>, String> {
        super::source_file::source_file_bytes(&self.app, &project_root, &path)
    }

    fn read_canvas_file(&self, content_ref: String) -> Result<Vec<u8>, String> {
        match super::canvas_files::read_canvas_file(&self.app, &content_ref) {
            Ok(Some((_, bytes))) => Ok(bytes),
            Ok(None) => Err(format!("canvas file {content_ref} was not found")),
            Err(error) => Err(error.to_string()),
        }
    }

    fn write_canvas_file(
        &self,
        artifact_id: String,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<serde_json::Value, String> {
        let reference =
            super::canvas_files::put_canvas_file(&self.app, &artifact_id, &media_type, &bytes)
                .map_err(|error| error.to_string())?;
        serde_json::to_value(reference).map_err(|error| error.to_string())
    }

    fn app_bridge_request(
        &self,
        request: DesktopAppBridgeRequest,
    ) -> Result<DesktopAppBridgeResponse, String> {
        let (status, body) = self.app.route_app_bridge_request(
            &request.method,
            &request.path,
            request.query,
            request.body.unwrap_or_default().into_bytes(),
        );
        Ok(DesktopAppBridgeResponse { status, body })
    }
}

#[derive(Debug)]
pub(crate) struct PendingDesktopBridge {
    delegate: OnceLock<Arc<AppCoreDesktopBridge>>,
    startup_frontend_actions: Mutex<Vec<serde_json::Value>>,
    pending_frontend_spawn: Mutex<Option<WorkArea>>,
    phase_marker: Mutex<Option<(String, Instant)>>,
    /// Synchronizes `subscribe_voice_playback` with the one delegate install.
    /// The subscription may start before startup finishes and must park rather
    /// than fail fast. `delegate` remains the sole readiness authority.
    ready_gate: Mutex<()>,
    ready_signal: Condvar,
}

impl Default for PendingDesktopBridge {
    fn default() -> Self {
        Self {
            delegate: OnceLock::new(),
            startup_frontend_actions: Mutex::new(Vec::new()),
            pending_frontend_spawn: Mutex::new(None),
            phase_marker: Mutex::new(None),
            ready_gate: Mutex::new(()),
            ready_signal: Condvar::new(),
        }
    }
}

impl PendingDesktopBridge {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn install(&self, delegate: Arc<AppCoreDesktopBridge>) -> Result<(), String> {
        // Hold the same gate used by waiters while publishing the OnceLock.
        // A waiter therefore cannot observe `None`, miss this notification,
        // and park after installation.
        let ready_gate_guard = self
            .ready_gate
            .lock()
            .map_err(|_| self.ready_gate_poisoned_error("install"))?;
        self.delegate.set(delegate.clone()).map_err(|_| {
            AppCoreError::unsupported("pending startup bridge", "single delegate install")
                .to_string()
        })?;
        self.ready_signal.notify_all();
        drop(ready_gate_guard);

        let mut pending = self
            .startup_frontend_actions
            .lock()
            .map_err(|_| "pending startup frontend-actions lock poisoned".to_string())?;
        for action in pending.drain(..) {
            delegate
                .app
                .enqueue_frontend_action(action)
                .map_err(|error| error.to_string())?;
        }

        let pending_work_area = self
            .pending_frontend_spawn
            .lock()
            .map_err(|_| "pending startup work area lock poisoned".to_string())?
            .take();
        if let Some(work_area) = pending_work_area {
            delegate.frontend_spawned(work_area)?;
        }
        Ok(())
    }

    pub(crate) fn record_startup_phase(
        &self,
        phase: &str,
        message: Option<&str>,
    ) -> Result<(), String> {
        if let Ok(mut marker) = self.phase_marker.lock() {
            let now = Instant::now();
            if let Some((previous_phase, started)) = marker.take() {
                crate::observability::record_startup_phase_duration(&previous_phase, now - started);
            }
            *marker = Some((phase.to_string(), now));
        }
        let mut action = json!({ "action": "frontend.startup_phase", "phase": phase });
        if let Some(message) = message {
            action["message"] = serde_json::Value::String(message.to_string());
        }
        self.enqueue_frontend_action(action)
    }

    fn startup_not_ready_error(&self, method: &str) -> String {
        format!("app is starting; request `{method}` is unavailable until startup completes",)
    }

    fn ready_delegate(&self) -> Option<&Arc<AppCoreDesktopBridge>> {
        self.delegate.get()
    }

    /// Waits for `install` to publish the delegate, parking on
    /// `ready_signal` under `ready_gate` rather than failing fast. This is
    /// used by `subscribe_voice_playback` only: that subscription is
    /// process-lifetime and is allowed to start before startup completes.
    /// Every other `PendingDesktopBridge` method keeps using `with_ready`
    /// and must keep rejecting calls until startup finishes.
    fn wait_for_ready_delegate(&self, method: &str) -> Result<Arc<AppCoreDesktopBridge>, String> {
        let mut ready_gate = self
            .ready_gate
            .lock()
            .map_err(|_| self.ready_gate_poisoned_error(method))?;
        loop {
            if let Some(delegate) = self.ready_delegate() {
                return Ok(Arc::clone(delegate));
            }
            ready_gate = self
                .ready_signal
                .wait(ready_gate)
                .map_err(|_| self.ready_gate_poisoned_error(method))?;
        }
    }

    fn ready_gate_poisoned_error(&self, method: &str) -> String {
        format!(
            "pending desktop bridge readiness lock poisoned while waiting for `{method}`; expected ready delegate after startup install"
        )
    }

    fn enqueue_frontend_action(&self, action: serde_json::Value) -> Result<(), String> {
        if let Some(delegate) = self.ready_delegate() {
            return delegate
                .app
                .enqueue_frontend_action(action)
                .map_err(|error| error.to_string());
        }

        self.startup_frontend_actions
            .lock()
            .map_err(|_| "pending startup frontend-actions lock poisoned".to_string())?
            .push(action);
        Ok(())
    }

    fn with_ready<R>(
        &self,
        method: &str,
        action: impl FnOnce(&AppCoreDesktopBridge) -> Result<R, String>,
    ) -> Result<R, String> {
        self.ready_delegate().map_or_else(
            || Err(self.startup_not_ready_error(method)),
            |delegate| action(delegate),
        )
    }
}

impl DesktopSemanticGraphBridge for PendingDesktopBridge {
    fn wait_for_storage_revision(&self, after_revision: i64) -> Result<i64, String> {
        self.with_ready("wait_for_storage_revision", |delegate| {
            delegate.wait_for_storage_revision(after_revision)
        })
    }

    fn list_indexed_semantic_graph_roots(&self) -> Result<Vec<String>, String> {
        self.with_ready("list_indexed_semantic_graph_roots", |delegate| {
            delegate.list_indexed_semantic_graph_roots()
        })
    }

    fn project_indexed_semantic_graph(
        &self,
        request: DesktopSemanticGraphRequest,
    ) -> Result<CompactSemanticGraphProjection, String> {
        self.with_ready("project_indexed_semantic_graph", |delegate| {
            delegate.project_indexed_semantic_graph(request)
        })
    }

    fn read_source_file_bytes(
        &self,
        project_root: String,
        path: String,
    ) -> Result<Vec<u8>, String> {
        self.with_ready("read_source_file_bytes", |delegate| {
            delegate.read_source_file_bytes(project_root, path)
        })
    }

    fn read_canvas_file(&self, content_ref: String) -> Result<Vec<u8>, String> {
        self.with_ready("read_canvas_file", |delegate| {
            delegate.read_canvas_file(content_ref)
        })
    }

    fn write_canvas_file(
        &self,
        artifact_id: String,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<serde_json::Value, String> {
        self.with_ready("write_canvas_file", |delegate| {
            delegate.write_canvas_file(artifact_id, media_type, bytes)
        })
    }

    fn app_bridge_request(
        &self,
        request: DesktopAppBridgeRequest,
    ) -> Result<DesktopAppBridgeResponse, String> {
        self.with_ready("app_bridge_request", |delegate| {
            delegate.app_bridge_request(request)
        })
    }
}

impl DesktopSettingsBridge for PendingDesktopBridge {
    fn bulb_visible(&self) -> Result<bool, String> {
        self.with_ready("bulb_visible", |delegate| delegate.bulb_visible())
    }

    fn apply_app_settings_patch(&self, patch: &AppSettingsPatch) -> Result<(), String> {
        self.with_ready("apply_app_settings_patch", |delegate| {
            delegate.apply_app_settings_patch(patch)
        })
    }

    fn set_provider_api_key(
        &self,
        provider_id: String,
        api_key: String,
    ) -> Result<serde_json::Value, String> {
        self.with_ready("set_provider_api_key", |delegate| {
            delegate.set_provider_api_key(provider_id, api_key)
        })
    }

    fn clear_provider_api_key(&self, provider_id: String) -> Result<serde_json::Value, String> {
        self.with_ready("clear_provider_api_key", |delegate| {
            delegate.clear_provider_api_key(provider_id)
        })
    }

    fn set_provider_endpoint(
        &self,
        provider_id: String,
        endpoint: String,
    ) -> Result<serde_json::Value, String> {
        self.with_ready("set_provider_endpoint", |delegate| {
            delegate.set_provider_endpoint(provider_id, endpoint)
        })
    }

    fn clear_provider_endpoint(&self, provider_id: String) -> Result<serde_json::Value, String> {
        self.with_ready("clear_provider_endpoint", |delegate| {
            delegate.clear_provider_endpoint(provider_id)
        })
    }

    fn frontend_spawned(&self, work_area: WorkArea) -> Result<(), String> {
        let Some(delegate) = self.ready_delegate() else {
            *self
                .pending_frontend_spawn
                .lock()
                .map_err(|_| "pending startup work area lock poisoned".to_string())? =
                Some(work_area);
            return Ok(());
        };
        delegate.frontend_spawned(work_area)
    }

    fn drain_frontend_actions(&self) -> Result<Vec<serde_json::Value>, String> {
        let Some(delegate) = self.ready_delegate() else {
            // Clone — the actual drain happens in install() which forwards to the app.
            return Ok(self
                .startup_frontend_actions
                .lock()
                .map_err(|_| "pending startup frontend-actions lock poisoned".to_string())?
                .clone());
        };
        delegate.drain_frontend_actions()
    }

    fn drain_window_actions(&self, window_label: &str) -> Result<Vec<serde_json::Value>, String> {
        match self.ready_delegate() {
            Some(delegate) => delegate.drain_window_actions(window_label),
            None if window_label == "lumvise-frontend" => self.drain_frontend_actions(),
            None => Ok(Vec::new()),
        }
    }

    fn assistant_provider_catalog(&self) -> Result<AssistantProviderCatalog, String> {
        self.with_ready("assistant_provider_catalog", |delegate| {
            delegate.assistant_provider_catalog()
        })
    }

    fn refresh_assistant_provider_catalog(&self) -> Result<AssistantProviderCatalog, String> {
        self.with_ready("refresh_assistant_provider_catalog", |delegate| {
            delegate.refresh_assistant_provider_catalog()
        })
    }

    fn assistant_selection(&self) -> Result<(String, Option<String>), String> {
        self.with_ready("assistant_selection", |delegate| {
            delegate.assistant_selection()
        })
    }

    fn compiled_plugin_views(&self) -> Result<serde_json::Value, String> {
        if self.ready_delegate().is_some() {
            return self.with_ready("compiled_plugin_views", |delegate| {
                delegate.compiled_plugin_views()
            });
        }
        Ok(json!({"baseUrl": null, "views": []}))
    }

    fn read_plugin_view_asset(
        &self,
        plugin_id: &str,
        view_id: &str,
        relative_path: &str,
    ) -> Result<DesktopPluginViewAsset, String> {
        self.with_ready("read_plugin_view_asset", |delegate| {
            delegate.read_plugin_view_asset(plugin_id, view_id, relative_path)
        })
    }

    fn invoke_compiled_view_host_api(
        &self,
        plugin_id: String,
        view_id: String,
        api_id: String,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.with_ready("invoke_compiled_view_host_api", |delegate| {
            delegate.invoke_compiled_view_host_api(plugin_id, view_id, api_id, input)
        })
    }

    fn managed_model_snapshot(&self) -> Result<serde_json::Value, String> {
        self.with_ready("managed_model_snapshot", |delegate| {
            delegate.managed_model_snapshot()
        })
    }

    fn select_managed_model(&self, model_id: String) -> Result<serde_json::Value, String> {
        self.with_ready("select_managed_model", |delegate| {
            delegate.select_managed_model(model_id.clone())
        })
    }

    fn retry_managed_model(&self, model_id: String) -> Result<serde_json::Value, String> {
        self.with_ready("retry_managed_model", |delegate| {
            delegate.retry_managed_model(model_id.clone())
        })
    }

    fn provider_credential_status(&self) -> Result<serde_json::Value, String> {
        self.with_ready("provider_credential_status", |delegate| {
            delegate.provider_credential_status()
        })
    }
}

impl DesktopWhiteboardBridge for PendingDesktopBridge {
    fn sync_user_canvas_scene(
        &self,
        session_id: String,
        canvas_id: Option<&str>,
        scene_json: String,
    ) -> Result<serde_json::Value, String> {
        self.with_ready("sync_user_canvas_scene", |delegate| {
            delegate.sync_user_canvas_scene(session_id, canvas_id, scene_json)
        })
    }
}

impl DesktopSettingsBridge for AppCoreDesktopBridge {
    fn bulb_visible(&self) -> Result<bool, String> {
        self.app
            .frontend()
            .app_settings()
            .map(|settings| settings.bulb_visible)
            .map_err(|error| error.to_string())
    }

    fn frontend_spawned(&self, work_area: WorkArea) -> Result<(), String> {
        self.app
            .frontend()
            .spawn_app(work_area)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn drain_frontend_actions(&self) -> Result<Vec<serde_json::Value>, String> {
        self.app
            .drain_frontend_actions()
            .map_err(|error| error.to_string())
    }

    fn drain_window_actions(&self, window_label: &str) -> Result<Vec<serde_json::Value>, String> {
        self.app
            .drain_window_actions(window_label)
            .map_err(|error| error.to_string())
    }

    fn assistant_provider_catalog(&self) -> Result<AssistantProviderCatalog, String> {
        self.app
            .frontend()
            .assistant_provider_catalog()
            .map_err(|error| error.to_string())
    }

    fn refresh_assistant_provider_catalog(&self) -> Result<AssistantProviderCatalog, String> {
        let sync = super::provider_startup::synchronize_llm_providers(self.app.relational.as_ref())
            .map_err(|error| error.to_string())?;
        let catalog = assistant_provider_catalog(&sync.catalog);
        self.app
            .replace_synchronized_providers(sync)
            .map_err(|error| error.to_string())?;
        self.app
            .frontend()
            .replace_assistant_provider_catalog(catalog.clone())
            .map_err(|error| error.to_string())?;
        Ok(catalog)
    }

    fn assistant_selection(&self) -> Result<(String, Option<String>), String> {
        let settings = self
            .app
            .frontend()
            .app_settings()
            .map_err(|error| error.to_string())?;
        Ok((
            settings.assistant_engine.value().to_string(),
            settings.assistant_model,
        ))
    }

    fn compiled_plugin_views(&self) -> Result<serde_json::Value, String> {
        let Some(server) = self._scoped_plugin_mcp_server.as_ref() else {
            return Ok(json!({"baseUrl": null, "views": []}));
        };
        let views = self
            .app
            .plugin_endpoints()
            .compiled_plugin_views()
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "baseUrl": server.base_url(),
            "views": views,
        }))
    }

    fn read_plugin_view_asset(
        &self,
        plugin_id: &str,
        view_id: &str,
        relative_path: &str,
    ) -> Result<DesktopPluginViewAsset, String> {
        let asset = self
            .app
            .plugin_system()
            .read_view_asset(plugin_id, view_id, relative_path)
            .map_err(|error| error.to_string())?;
        if asset.surface != lumvise_plugin_package::ViewSurface::NativeWindow {
            return Err(format!(
                "View {plugin_id}/{view_id}: expected signed native_window surface"
            ));
        }
        Ok(DesktopPluginViewAsset {
            path: asset.asset_path,
            bytes: asset.bytes,
            content_security_policy: asset.content_security_policy,
        })
    }

    fn invoke_compiled_view_host_api(
        &self,
        plugin_id: String,
        view_id: String,
        api_id: String,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let context = crate::plugin::surface_invocation_context(
            "desktop-view-host-api",
            Instant::now() + Duration::from_secs(60),
        );
        self.app
            .plugin_system()
            .invoke_view_host_api_controlled(&plugin_id, &view_id, &api_id, input, context)
            .map_err(|error| error.to_string())
    }

    fn apply_app_settings_patch(&self, patch: &AppSettingsPatch) -> Result<(), String> {
        self.app
            .frontend()
            .apply_app_settings_patch(patch)
            .map(|_| ())
            .map_err(|error| error.to_string())?;
        // The settings window writes through this bridge; without the durable
        // write the panel holds the patch in memory only and every restart
        // restores the previously persisted selection.
        self.app
            .frontend()
            .persist_app_settings()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn managed_model_snapshot(&self) -> Result<serde_json::Value, String> {
        let manager = self
            .app
            .managed_models()
            .ok_or_else(|| "managed models are not installed".to_string())?;
        let mut snapshot =
            serde_json::to_value(manager.snapshot()).map_err(|error| error.to_string())?;
        snapshot["storageRoot"] = json!(manager.models_root().display().to_string());
        Ok(snapshot)
    }

    fn select_managed_model(&self, model_id: String) -> Result<serde_json::Value, String> {
        let manager = self
            .app
            .managed_models()
            .ok_or_else(|| "managed models are not installed".to_string())?;
        let status = manager
            .select(&model_id)
            .map_err(|error| error.to_string())?;
        serde_json::to_value(status).map_err(|error| error.to_string())
    }

    fn retry_managed_model(&self, model_id: String) -> Result<serde_json::Value, String> {
        let manager = self
            .app
            .managed_models()
            .ok_or_else(|| "managed models are not installed".to_string())?;
        let status = manager
            .retry(&model_id)
            .map_err(|error| error.to_string())?;
        serde_json::to_value(status).map_err(|error| error.to_string())
    }

    fn provider_credential_status(&self) -> Result<serde_json::Value, String> {
        super::provider_settings::provider_credential_status(self.app.relational.as_ref())
            .map_err(|error| error.to_string())
    }

    fn set_provider_api_key(
        &self,
        provider_id: String,
        api_key: String,
    ) -> Result<serde_json::Value, String> {
        let routing = lumvise_resource_routing::ResourceRoutingConfig::from_environment()
            .map_err(|error| error.to_string())?;
        if routing.llm_execution == lumvise_resource_routing::ResourcePlacement::Centralized {
            return Err(
                "desktop provider credentials are unavailable for centralized LLM execution"
                    .to_string(),
            );
        }
        super::provider_settings::set_provider_api_key(
            self.app.relational.as_ref(),
            &provider_id,
            &api_key,
        )
        .map_err(|error| error.to_string())?;
        let sync = super::provider_startup::synchronize_llm_providers(self.app.relational.as_ref())
            .map_err(|error| error.to_string())?;
        let catalog = assistant_provider_catalog(&sync.catalog);
        self.app
            .replace_synchronized_providers(sync)
            .map_err(|error| error.to_string())?;
        self.app
            .frontend()
            .replace_assistant_provider_catalog(catalog.clone())
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "providerId": provider_id,
            "saved": true,
            "catalog": catalog,
        }))
    }

    fn clear_provider_api_key(&self, provider_id: String) -> Result<serde_json::Value, String> {
        let routing = lumvise_resource_routing::ResourceRoutingConfig::from_environment()
            .map_err(|error| error.to_string())?;
        if routing.llm_execution == lumvise_resource_routing::ResourcePlacement::Centralized {
            return Err(
                "desktop provider credentials are unavailable for centralized LLM execution"
                    .to_string(),
            );
        }
        super::provider_settings::clear_provider_api_key(
            self.app.relational.as_ref(),
            &provider_id,
        )
        .map_err(|error| error.to_string())?;
        let sync = super::provider_startup::synchronize_llm_providers(self.app.relational.as_ref())
            .map_err(|error| error.to_string())?;
        let catalog = assistant_provider_catalog(&sync.catalog);
        self.app
            .replace_synchronized_providers(sync)
            .map_err(|error| error.to_string())?;
        self.app
            .frontend()
            .replace_assistant_provider_catalog(catalog.clone())
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "providerId": provider_id,
            "cleared": true,
            "catalog": catalog,
        }))
    }

    fn set_provider_endpoint(
        &self,
        provider_id: String,
        endpoint: String,
    ) -> Result<serde_json::Value, String> {
        let routing = lumvise_resource_routing::ResourceRoutingConfig::from_environment()
            .map_err(|error| error.to_string())?;
        if routing.llm_execution == lumvise_resource_routing::ResourcePlacement::Centralized {
            return Err(
                "desktop provider endpoints are unavailable for centralized LLM execution"
                    .to_string(),
            );
        }
        super::provider_settings::set_provider_endpoint(
            self.app.relational.as_ref(),
            &provider_id,
            &endpoint,
        )
        .map_err(|error| error.to_string())?;
        let sync = super::provider_startup::synchronize_llm_providers(self.app.relational.as_ref())
            .map_err(|error| error.to_string())?;
        let catalog = assistant_provider_catalog(&sync.catalog);
        self.app
            .replace_synchronized_providers(sync)
            .map_err(|error| error.to_string())?;
        self.app
            .frontend()
            .replace_assistant_provider_catalog(catalog.clone())
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "providerId": provider_id,
            "saved": true,
            "catalog": catalog,
        }))
    }

    fn clear_provider_endpoint(&self, provider_id: String) -> Result<serde_json::Value, String> {
        let routing = lumvise_resource_routing::ResourceRoutingConfig::from_environment()
            .map_err(|error| error.to_string())?;
        if routing.llm_execution == lumvise_resource_routing::ResourcePlacement::Centralized {
            return Err(
                "desktop provider endpoints are unavailable for centralized LLM execution"
                    .to_string(),
            );
        }
        super::provider_settings::clear_provider_endpoint(
            self.app.relational.as_ref(),
            &provider_id,
        )
        .map_err(|error| error.to_string())?;
        let sync = super::provider_startup::synchronize_llm_providers(self.app.relational.as_ref())
            .map_err(|error| error.to_string())?;
        let catalog = assistant_provider_catalog(&sync.catalog);
        self.app
            .replace_synchronized_providers(sync)
            .map_err(|error| error.to_string())?;
        self.app
            .frontend()
            .replace_assistant_provider_catalog(catalog.clone())
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "providerId": provider_id,
            "cleared": true,
            "catalog": catalog,
        }))
    }
}

impl DesktopWhiteboardBridge for AppCoreDesktopBridge {
    fn sync_user_canvas_scene(
        &self,
        session_id: String,
        canvas_id: Option<&str>,
        scene_json: String,
    ) -> Result<serde_json::Value, String> {
        let scene = serde_json::from_str(&scene_json).map_err(|error| {
            format!("invalid user canvas scene for session {session_id:?}: {error}")
        })?;
        let canvas = self
            .app
            .frontend()
            .replace_canvas_scene(
                canvas_id.unwrap_or(lumvise_frontend_core::MAIN_CANVAS_ID),
                scene,
                "user",
            )
            .map_err(|error| error.to_string())?;
        serde_json::to_value(canvas).map_err(|error| error.to_string())
    }
}

fn incomplete_voice_snippet_result() -> serde_json::Value {
    json!({
        "transcript": null,
        "finalTranscript": null,
        "completed": false,
        "transcription": { "backend": "app-core", "speechDetected": false }
    })
}

fn voice_snippet_result(
    request_id: String,
    transcript: String,
    completed: bool,
) -> serde_json::Value {
    json!({
        "transcript": transcript,
        "finalTranscript": if completed { serde_json::Value::String(transcript.clone()) } else { serde_json::Value::Null },
        "completed": completed,
        "transcription": { "backend": "app-core", "speechDetected": true },
        "requestId": request_id
    })
}

fn transcription_result(
    request_id: String,
    transcript: String,
    options: Option<serde_json::Value>,
) -> serde_json::Value {
    json!({
        "transcript": transcript,
        "speechDetected": !transcript.trim().is_empty(),
        "levels": observed_levels(options),
        "suggestedThresholds": default_voice_thresholds(),
        "retainedCapturePath": format!("app-core://{request_id}")
    })
}

fn observed_levels(options: Option<serde_json::Value>) -> serde_json::Value {
    options
        .and_then(|value| value.get("observedLevels").cloned())
        .unwrap_or(serde_json::Value::Null)
}

fn default_voice_thresholds() -> serde_json::Value {
    json!({
        "silenceRmsThreshold": 0.001,
        "silencePeakThreshold": 0.002,
        "speechStartRmsThreshold": 0.01,
        "meaningfulSpeechRmsThreshold": 0.02,
        "meaningfulSpeechPeakThreshold": 0.03
    })
}

fn sample_rate_from_value(value: &serde_json::Value) -> Result<u32, String> {
    value
        .get("sampleRate")
        .and_then(serde_json::Value::as_u64)
        .and_then(|sample_rate| u32::try_from(sample_rate).ok())
        .ok_or_else(|| "sampleRate must be a u32 audio sample rate".to_string())
}

fn samples_from_value(value: &serde_json::Value) -> Result<Vec<f32>, String> {
    let samples = value
        .get("samples")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "samples must be an array of f32 audio samples".to_string())?;
    samples.iter().map(sample_from_value).collect()
}

fn sample_from_value(value: &serde_json::Value) -> Result<f32, String> {
    value
        .as_f64()
        .map(|sample| sample as f32)
        .ok_or_else(|| format!("sample must be a number, got {value}"))
}

fn encode_pcm16_wav(samples: Vec<f32>, sample_rate: u32) -> Result<Vec<u8>, String> {
    if sample_rate == 0 {
        return Err("sample_rate must be a positive u32 audio sample rate".to_string());
    }
    let data_len = samples
        .len()
        .checked_mul(2)
        .ok_or_else(|| "samples length overflows WAV data size".to_string())?;
    let mut bytes = wav_header(data_len, sample_rate)?;
    for sample in samples {
        bytes.extend_from_slice(&pcm16_sample(sample).to_le_bytes());
    }
    Ok(bytes)
}

fn wav_header(data_len: usize, sample_rate: u32) -> Result<Vec<u8>, String> {
    let data_len = u32::try_from(data_len).map_err(|_| "WAV data exceeds u32 size".to_string())?;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    Ok(bytes)
}

fn pcm16_sample(sample: f32) -> i16 {
    let clamped = sample.clamp(-1.0, 1.0);
    (clamped * f32::from(i16::MAX)).round() as i16
}

mod voice_transport;

#[cfg(test)]
mod tests;
