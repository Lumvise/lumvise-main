use crate::{AppCore, AppCoreError, Result};
use lumvise_db_core::SettingRecord;
use lumvise_frontend_core::{
    AppSettings, AppSettingsPatch, AssistantProviderCatalog, CanvasPatch, CanvasSnapshot,
    DashboardView, FrontendRuntimeSnapshot, FrontendStatus, MAIN_CANVAS_ID, WorkArea,
};
use serde_json::Value;

const FRONTEND_SETTINGS_SCOPE: &str = "frontend";
const APP_SETTINGS_KEY: &str = "app_settings";

pub struct FrontendInteractionEndpoints<'app> {
    app: &'app AppCore,
}

impl<'app> FrontendInteractionEndpoints<'app> {
    pub(crate) fn new(app: &'app AppCore) -> Self {
        Self { app }
    }

    /// Spawns the frontend in compact orb mode for a concrete work area.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// app.frontend().spawn_app(area).unwrap();
    /// ```
    pub fn spawn_app(&self, work_area: WorkArea) -> Result<FrontendRuntimeSnapshot> {
        let mut frontend = self.lock_frontend()?;
        Ok(frontend.spawn_app(work_area)?)
    }
    pub fn replace_assistant_provider_catalog(
        &self,
        catalog: AssistantProviderCatalog,
    ) -> Result<()> {
        let changed = self
            .lock_frontend()?
            .replace_assistant_provider_catalog(catalog);
        if changed {
            self.persist_app_settings()?;
        }
        Ok(())
    }

    /// Returns the active-source assistant provider catalog (TOML-backed).
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert_eq!(
    ///     app.frontend().assistant_provider_catalog().unwrap().providers.len(),
    ///     0,
    /// );
    /// ```
    pub fn assistant_provider_catalog(&self) -> Result<AssistantProviderCatalog> {
        Ok(self.lock_frontend()?.assistant_provider_catalog().clone())
    }

    /// Opens a dashboard view through Frontend Core state.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// let frontend = app.frontend();
    /// frontend.spawn_app(area).unwrap();
    /// frontend.open_dashboard_view(lumvise_frontend_core::DashboardView::CanvasDashboard).unwrap();
    /// ```
    pub fn open_dashboard_view(&self, view: DashboardView) -> Result<FrontendRuntimeSnapshot> {
        let mut frontend = self.lock_frontend()?;
        Ok(frontend.open_dashboard_view(view)?)
    }

    /// Updates the app canvas and returns the new canvas revision.
    ///
    /// Endpoint callers address the desktop whiteboard canvas
    /// ([`MAIN_CANVAS_ID`]); workspace conversation canvases go through the
    /// `frontend.canvas` host capability with an explicit `canvas_id`.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let patch = lumvise_app_core::CanvasPatch { canvas_id: "main".into(), elements: vec![] };
    /// assert_eq!(app.frontend().update_canvas(patch).unwrap().revision, 1);
    /// ```
    pub fn update_canvas(&self, patch: CanvasPatch) -> Result<CanvasSnapshot> {
        Ok(self.lock_frontend()?.update_canvas(MAIN_CANVAS_ID, patch)?)
    }

    /// Applies an RFC 6902 patch to the native Excalidraw canvas document.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let patch = serde_json::json!([
    ///   { "op": "add", "path": "/elementsById/card", "value": { "id": "card", "type": "rectangle" } },
    ///   { "op": "add", "path": "/elementOrder/0", "value": "card" }
    /// ]);
    /// let canvas = app.frontend().apply_canvas_diff("main", patch, "assistant").unwrap();
    /// assert_eq!(canvas.revision, 1);
    /// ```
    pub fn apply_canvas_diff(
        &self,
        canvas_id: &str,
        patch: Value,
        actor: &str,
    ) -> Result<CanvasSnapshot> {
        require_non_empty(canvas_id, "non-empty canvas id")?;
        require_non_empty(actor, "non-empty canvas actor")?;
        Ok(self
            .lock_frontend()?
            .apply_canvas_diff(canvas_id, patch, actor, None)?)
    }

    /// Replaces the native Excalidraw canvas scene and records the actor diff.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let scene = serde_json::json!({ "type": "excalidraw", "version": 2, "elements": [] });
    /// let canvas = app.frontend().replace_canvas_scene("main", scene, "user").unwrap();
    /// assert_eq!(canvas.last_updated_by, "user");
    /// ```
    pub fn replace_canvas_scene(
        &self,
        canvas_id: &str,
        scene: Value,
        actor: &str,
    ) -> Result<CanvasSnapshot> {
        require_non_empty(canvas_id, "non-empty canvas id")?;
        require_non_empty(actor, "non-empty canvas actor")?;
        Ok(self
            .lock_frontend()?
            .replace_canvas_scene(canvas_id, scene, actor)?)
    }

    /// Retrieves the latest canvas snapshot.
    ///
    /// Endpoint callers address the desktop whiteboard canvas
    /// ([`MAIN_CANVAS_ID`]); workspace conversation canvases go through the
    /// `frontend.canvas` host capability with an explicit `canvas_id`.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert_eq!(app.frontend().canvas().unwrap().canvas_id, "main");
    /// ```
    pub fn canvas(&self) -> Result<CanvasSnapshot> {
        Ok(self.lock_frontend()?.canvas(MAIN_CANVAS_ID))
    }

    /// Retrieves user-authored canvas diffs after a revision.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let changes = app.frontend().user_canvas_changes("main", 0).unwrap();
    /// assert_eq!(changes["changes"], serde_json::json!([]));
    /// ```
    pub fn user_canvas_changes(&self, canvas_id: &str, since_revision: u64) -> Result<Value> {
        require_non_empty(canvas_id, "non-empty canvas id")?;
        Ok(self
            .lock_frontend()?
            .user_canvas_changes(canvas_id, since_revision)?)
    }

    /// Retrieves the current frontend status for API or plugin consumers.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let _status = app.frontend().frontend_status().unwrap();
    /// ```
    pub fn frontend_status(&self) -> Result<FrontendStatus> {
        let mut status = self.lock_frontend()?.frontend_status();
        crate::plugin::compiled_surfaces::CompiledSurfaceCatalog::load(self.app)?
            .merge_views(&mut status.views)?;
        Ok(status)
    }

    /// Applies a frontend settings patch and persists the full settings document.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let record = app.frontend()
    ///     .apply_app_settings_patch(&lumvise_frontend_core::AppSettingsPatch::VoiceRecordingEnabled(false))
    ///     .unwrap();
    /// assert_eq!(record.scope, "frontend");
    /// ```
    pub fn apply_app_settings_patch(&self, patch: &AppSettingsPatch) -> Result<SettingRecord> {
        let mut frontend = self.lock_frontend()?;
        let mut proposed = frontend.app_settings().clone();
        proposed.apply_app_settings_patch(patch);
        let value = serde_json::to_value(&proposed).map_err(serialized_settings_error)?;
        // Publish only durable preferences. A failed Finish must not leak its
        // completion flag into a later, unrelated settings save.
        let record =
            self.app
                .database()
                .set_setting(FRONTEND_SETTINGS_SCOPE, APP_SETTINGS_KEY, &value)?;
        frontend.restore_app_settings(proposed);
        Ok(record)
    }

    /// Retrieves frontend App Settings from SQL or current memory state.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(app.frontend().app_settings().unwrap().voice_recording_enabled);
    /// ```
    pub fn app_settings(&self) -> Result<AppSettings> {
        let Some(record) = self.app_settings_record()? else {
            return Ok(self.lock_frontend()?.app_settings().clone());
        };
        app_settings_from_value(record.value)
    }

    /// Persists the current frontend App Settings to SQL.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let record = app.frontend().persist_app_settings().unwrap();
    /// assert_eq!(record.key, "app_settings");
    /// ```
    pub fn persist_app_settings(&self) -> Result<SettingRecord> {
        let frontend = self.lock_frontend()?;
        let value =
            serde_json::to_value(frontend.app_settings()).map_err(serialized_settings_error)?;
        self.app
            .database()
            .set_setting(FRONTEND_SETTINGS_SCOPE, APP_SETTINGS_KEY, &value)
    }

    /// Retrieves the raw SQL settings record for frontend App Settings.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(app.frontend().app_settings_record().unwrap().is_none());
    /// ```
    pub fn app_settings_record(&self) -> Result<Option<SettingRecord>> {
        self.app
            .database()
            .setting(FRONTEND_SETTINGS_SCOPE, APP_SETTINGS_KEY)
    }

    fn lock_frontend(
        &self,
    ) -> Result<std::sync::MutexGuard<'app, lumvise_frontend_core::FrontendCore>> {
        self.app
            .frontend
            .lock()
            .map_err(|_| AppCoreError::poisoned_mutex("frontend"))
    }
}

fn app_settings_from_value(value: Value) -> Result<AppSettings> {
    serde_json::from_value(value).map_err(serialized_settings_error)
}

fn serialized_settings_error(error: serde_json::Error) -> AppCoreError {
    AppCoreError::invalid_value(error.to_string(), "serialized AppSettings")
}

pub(crate) fn require_non_empty(value: &str, expected: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(AppCoreError::invalid_value(value, expected));
    }
    Ok(())
}
