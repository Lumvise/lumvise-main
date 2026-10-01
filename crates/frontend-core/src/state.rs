use std::collections::BTreeMap;

use crate::canvas::{CanvasSnapshot, MAIN_CANVAS_ID};
use crate::error::{FrontendError, Result};
use crate::modality::{ModalityStreamCatalog, ModalityStreamsInterface};
use crate::settings::{AppSettings, AssistantProviderCatalog};
use crate::types::{
    AppLifecycle, DashboardView, DashboardVisibility, OrbMode, OrbVisibility, SurfaceMode,
    WhiteboardSurface,
};
use crate::view_registry::ViewRegistry;
use crate::voice::{VoicePlaybackState, VoiceRecordingState};
use crate::window::{WindowLayout, WindowManagementPlan, WorkArea};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnedAppState {
    pub lifecycle: AppLifecycle,
    pub surface_mode: SurfaceMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrbWidgetState {
    pub mode: OrbMode,
    pub visibility: OrbVisibility,
    pub countdown_digit: Option<u8>,
    pub draggable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhiteboardState {
    pub surface: WhiteboardSurface,
    pub active_view: DashboardView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardState {
    pub visibility: DashboardVisibility,
    pub active_view: DashboardView,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontendRuntimeState {
    pub app: SpawnedAppState,
    pub orb: OrbWidgetState,
    pub whiteboard: WhiteboardState,
    pub dashboard: DashboardState,
    pub streams: ModalityStreamCatalog,
    pub voice_recording: VoiceRecordingState,
    pub voice_playback: VoicePlaybackState,
    pub views: ViewRegistry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrontendStatus {
    pub lifecycle: AppLifecycle,
    pub surface_mode: SurfaceMode,
    pub orb: OrbWidgetState,
    pub whiteboard: WhiteboardState,
    pub dashboard: DashboardState,
    pub streams: ModalityStreamCatalog,
    pub voice_recording: VoiceRecordingState,
    pub voice_playback: VoicePlaybackState,
    pub views: ViewRegistry,
    pub window_layout: Option<WindowLayout>,
    pub window_plan: Option<WindowManagementPlan>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrontendRuntimeSnapshot {
    pub state: FrontendRuntimeState,
    pub window_layout: WindowLayout,
    pub window_plan: WindowManagementPlan,
    pub settings: AppSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrontendCore {
    pub(crate) state: FrontendRuntimeState,
    pub(crate) window_layout: Option<WindowLayout>,
    pub(crate) settings: AppSettings,
    pub(crate) assistant_provider_catalog: AssistantProviderCatalog,
    /// One independent canvas per id; the desktop whiteboard owns
    /// [`MAIN_CANVAS_ID`], workspace conversations own their session id.
    pub(crate) canvases: BTreeMap<String, CanvasSnapshot>,
}

impl Default for SpawnedAppState {
    fn default() -> Self {
        Self {
            lifecycle: AppLifecycle::NotSpawned,
            surface_mode: SurfaceMode::Compact,
        }
    }
}

impl Default for OrbWidgetState {
    fn default() -> Self {
        Self {
            mode: OrbMode::Idle,
            visibility: OrbVisibility::Visible,
            countdown_digit: None,
            draggable: true,
        }
    }
}

impl Default for WhiteboardState {
    fn default() -> Self {
        Self {
            surface: WhiteboardSurface::Hidden,
            active_view: DashboardView::CanvasDashboard,
        }
    }
}

impl Default for DashboardState {
    fn default() -> Self {
        Self {
            visibility: DashboardVisibility::Hidden,
            active_view: DashboardView::CanvasDashboard,
        }
    }
}

impl FrontendCore {
    /// Creates frontend-core state using App Settings defaults.
    ///
    /// # Example
    ///
    /// ```
    /// let core = lumvise_frontend_core::FrontendCore::new(
    ///     lumvise_frontend_core::AppSettings::default(),
    /// );
    /// assert!(core.window_layout().is_none());
    /// ```
    pub fn new(settings: AppSettings) -> Self {
        let mut state = FrontendRuntimeState::default();
        state.dashboard.active_view = settings.default_dashboard_view.clone();
        state.whiteboard.active_view = settings.default_dashboard_view.clone();
        Self {
            assistant_provider_catalog: AssistantProviderCatalog::default(),
            state,
            window_layout: None,
            settings,
            canvases: BTreeMap::from([(
                MAIN_CANVAS_ID.to_string(),
                CanvasSnapshot::empty(MAIN_CANVAS_ID),
            )]),
        }
    }

    /// Returns the current frontend state.
    ///
    /// # Example
    ///
    /// ```
    /// let core = lumvise_frontend_core::FrontendCore::default();
    /// assert_eq!(core.state().orb.mode, lumvise_frontend_core::OrbMode::Idle);
    /// ```
    pub fn state(&self) -> &FrontendRuntimeState {
        &self.state
    }

    /// Returns the current Tauri window layout when the app is spawned.
    ///
    /// # Example
    ///
    /// ```
    /// let core = lumvise_frontend_core::FrontendCore::default();
    /// assert!(core.window_layout().is_none());
    /// ```
    pub fn window_layout(&self) -> Option<&WindowLayout> {
        self.window_layout.as_ref()
    }

    /// Rebuilds the current window shape for the supplied desktop work area.
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// core.spawn_app(lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0)).unwrap();
    /// core.set_window_work_area(lumvise_frontend_core::WorkArea::new(1440, 0, 1440, 900, 1.0)).unwrap();
    /// assert_eq!(core.window_layout().unwrap().work_area.x, 1440);
    /// ```
    pub fn set_window_work_area(&mut self, work_area: WorkArea) -> Result<()> {
        if work_area.width == 0 || work_area.height == 0 {
            return Err(FrontendError::invalid_value("0x0", "non-empty work area"));
        }
        self.require_spawned()?;
        self.window_layout = self
            .window_layout
            .map(|layout| layout.with_work_area(work_area));
        self.snapshot().map(|_| ())
    }

    /// Returns the state-machine-facing frontend status.
    ///
    /// # Example
    ///
    /// ```
    /// let core = lumvise_frontend_core::FrontendCore::default();
    /// assert_eq!(core.frontend_status().orb.visibility, lumvise_frontend_core::OrbVisibility::Visible);
    /// ```
    pub fn frontend_status(&self) -> FrontendStatus {
        FrontendStatus {
            lifecycle: self.state.app.lifecycle,
            surface_mode: self.state.app.surface_mode,
            orb: self.state.orb.clone(),
            whiteboard: self.state.whiteboard.clone(),
            dashboard: self.state.dashboard.clone(),
            streams: self.state.streams.clone(),
            voice_recording: self.state.voice_recording.clone(),
            voice_playback: self.state.voice_playback.clone(),
            views: self.state.views.clone(),
            window_layout: self.window_layout,
            window_plan: self.window_layout.map(WindowLayout::management_plan),
        }
    }

    /// Spawns the compact widget app in the supplied desktop work area.
    ///
    /// # Example
    ///
    /// ```
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// assert_eq!(core.spawn_app(area).unwrap().state.app.lifecycle, lumvise_frontend_core::AppLifecycle::Spawned);
    /// ```
    pub fn spawn_app(&mut self, work_area: WorkArea) -> Result<FrontendRuntimeSnapshot> {
        if work_area.width == 0 || work_area.height == 0 {
            return Err(FrontendError::invalid_value("0x0", "non-empty work area"));
        }
        self.window_layout = Some(WindowLayout::compact(work_area));
        self.state.app.lifecycle = AppLifecycle::Spawned;
        self.state.orb.visibility = OrbVisibility::Visible;
        self.apply_whiteboard_surface(WhiteboardSurface::Hidden);
        self.snapshot()
    }

    /// Opens the dashboard shell as the effect of clicking the orb.
    ///
    /// # Example
    ///
    /// ```
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// core.spawn_app(area).unwrap();
    /// assert_eq!(core.click_orb().unwrap().state.dashboard.visibility, lumvise_frontend_core::DashboardVisibility::Visible);
    /// ```
    pub fn click_orb(&mut self) -> Result<FrontendRuntimeSnapshot> {
        self.require_spawned()?;
        self.apply_whiteboard_surface(WhiteboardSurface::Shell);
        self.state.orb.mode = OrbMode::Activity;
        self.snapshot()
    }

    /// Hides the dashboard and returns the Tauri window to compact orb mode.
    ///
    /// # Example
    ///
    /// ```
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// core.spawn_app(area).unwrap();
    /// core.click_orb().unwrap();
    /// assert_eq!(core.close_dashboard().unwrap().state.app.surface_mode, lumvise_frontend_core::SurfaceMode::Compact);
    /// ```
    pub fn close_dashboard(&mut self) -> Result<FrontendRuntimeSnapshot> {
        self.require_spawned()?;
        self.apply_whiteboard_surface(WhiteboardSurface::Hidden);
        self.state.orb.mode = OrbMode::Idle;
        self.snapshot()
    }

    /// Switches the visible dashboard to a supported view.
    ///
    /// # Example
    ///
    /// ```
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// core.spawn_app(area).unwrap();
    /// assert_eq!(core.open_dashboard_view(lumvise_frontend_core::DashboardView::GraphViewer).unwrap().state.dashboard.active_view, lumvise_frontend_core::DashboardView::GraphViewer);
    /// ```
    pub fn open_dashboard_view(&mut self, view: DashboardView) -> Result<FrontendRuntimeSnapshot> {
        self.require_view_enabled(&view)?;
        self.click_orb()?;
        self.state.dashboard.active_view = view.clone();
        self.state.whiteboard.active_view = view;
        self.snapshot()
    }

    /// Sets whether the orb is visible to the desktop shell.
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// core.spawn_app(lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0)).unwrap();
    /// assert_eq!(core.set_orb_visibility(lumvise_frontend_core::OrbVisibility::Hidden).unwrap().state.orb.visibility, lumvise_frontend_core::OrbVisibility::Hidden);
    /// ```
    pub fn set_orb_visibility(
        &mut self,
        visibility: OrbVisibility,
    ) -> Result<FrontendRuntimeSnapshot> {
        self.require_spawned()?;
        self.state.orb.visibility = visibility;
        self.snapshot()
    }

    /// Sets the orb mode for state-machine controlled visual status.
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// core.spawn_app(lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0)).unwrap();
    /// assert_eq!(core.set_orb_mode(lumvise_frontend_core::OrbMode::Session).unwrap().state.orb.mode, lumvise_frontend_core::OrbMode::Session);
    /// ```
    pub fn set_orb_mode(&mut self, mode: OrbMode) -> Result<FrontendRuntimeSnapshot> {
        if mode == OrbMode::Countdown {
            return Err(FrontendError::invalid_value(
                "countdown",
                "start_countdown(digit) for countdown mode",
            ));
        }
        self.require_spawned()?;
        self.state.orb.mode = mode;
        self.state.orb.countdown_digit = None;
        self.snapshot()
    }

    /// Sets the whiteboard surface state and matching Tauri layout plan.
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// core.spawn_app(lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0)).unwrap();
    /// assert_eq!(core.set_whiteboard_surface(lumvise_frontend_core::WhiteboardSurface::Shell).unwrap().state.whiteboard.surface, lumvise_frontend_core::WhiteboardSurface::Shell);
    /// ```
    pub fn set_whiteboard_surface(
        &mut self,
        surface: WhiteboardSurface,
    ) -> Result<FrontendRuntimeSnapshot> {
        self.require_spawned()?;
        self.apply_whiteboard_surface(surface);
        self.snapshot()
    }

    /// Moves the orb into countdown state before a voice/session turn.
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// core.spawn_app(lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0)).unwrap();
    /// assert_eq!(core.start_countdown(3).unwrap().state.orb.countdown_digit, Some(3));
    /// ```
    pub fn start_countdown(&mut self, digit: u8) -> Result<FrontendRuntimeSnapshot> {
        if !(1..=9).contains(&digit) {
            return Err(FrontendError::invalid_value(
                digit.to_string(),
                "digit 1..=9",
            ));
        }
        self.require_spawned()?;
        self.state.orb.mode = OrbMode::Countdown;
        self.state.orb.visibility = OrbVisibility::Visible;
        self.state.orb.countdown_digit = Some(digit);
        self.snapshot()
    }

    pub fn modality_streams(&self) -> ModalityStreamsInterface {
        self.state.streams.interface()
    }

    pub(crate) fn require_spawned(&self) -> Result<()> {
        if self.state.app.lifecycle != AppLifecycle::Spawned {
            return Err(FrontendError::invalid_value("not_spawned", "spawned app"));
        }
        Ok(())
    }

    pub(crate) fn require_view_enabled(&self, view: &DashboardView) -> Result<()> {
        if *view == DashboardView::GraphViewer && !self.settings.graph_view_enabled {
            return Err(FrontendError::invalid_value(
                "graph_viewer",
                "enabled graph view",
            ));
        }
        if *view == DashboardView::VoiceRecording && !self.settings.voice_recording_enabled {
            return Err(FrontendError::invalid_value(
                "voice_recording",
                "enabled voice recording",
            ));
        }
        Ok(())
    }

    fn update_window_mode(&mut self, mode: SurfaceMode) {
        self.window_layout = self
            .window_layout
            .map(|layout| layout.with_surface_mode(mode));
    }

    pub(crate) fn apply_whiteboard_surface(&mut self, surface: WhiteboardSurface) {
        self.state.whiteboard.surface = surface;
        self.state.dashboard.visibility = dashboard_visibility_for(surface);
        self.state.app.surface_mode = surface_mode_for(surface);
        self.update_window_mode(surface_mode_for(surface));
    }

    pub(crate) fn snapshot(&self) -> Result<FrontendRuntimeSnapshot> {
        let Some(window_layout) = self.window_layout else {
            return Err(FrontendError::MissingValue {
                value: "window_layout".to_string(),
                expected: "spawned app window layout".to_string(),
            });
        };
        Ok(FrontendRuntimeSnapshot {
            state: self.state.clone(),
            window_layout,
            window_plan: window_layout.management_plan(),
            settings: self.settings.clone(),
        })
    }
}

impl Default for FrontendCore {
    fn default() -> Self {
        Self::new(AppSettings::default())
    }
}

fn dashboard_visibility_for(surface: WhiteboardSurface) -> DashboardVisibility {
    match surface {
        WhiteboardSurface::Hidden => DashboardVisibility::Hidden,
        WhiteboardSurface::Shell | WhiteboardSurface::Fullscreen => DashboardVisibility::Visible,
    }
}

fn surface_mode_for(surface: WhiteboardSurface) -> SurfaceMode {
    match surface {
        WhiteboardSurface::Hidden => SurfaceMode::Compact,
        WhiteboardSurface::Shell => SurfaceMode::Shell,
        WhiteboardSurface::Fullscreen => SurfaceMode::Fullscreen,
    }
}
