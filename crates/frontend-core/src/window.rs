use crate::types::SurfaceMode;
use serde::{Deserialize, Serialize};

pub const COMPACT_WIDGET_SIZE: u32 = 135;
pub const COMPACT_OFFSET_X: i32 = 20;
pub const COMPACT_OFFSET_Y: i32 = 28;
pub const SHELL_WIDTH: u32 = 1180;
pub const SHELL_HEIGHT: u32 = 760;
pub const MINIMIZE_RESTORE_DELAY_MS: u64 = 120;
pub const HOST_MINIMIZE_COLLAPSE_SCRIPT: &str = r#"
window.lumwise?.setExpanded?.(false).catch((error) => {
  console.warn("Lumvise host minimize collapse failed", error);
});
"#;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WorkArea {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidgetBounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowLayout {
    pub work_area: WorkArea,
    pub active_bounds: WidgetBounds,
    pub surface_mode: SurfaceMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowDisplayState {
    CompactOrb,
    DashboardShell,
    DashboardFullscreen,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum WindowManagementCommand {
    Unminimize,
    Unmaximize,
    Maximize,
    SetBounds { bounds: WidgetBounds },
    RememberCompactBounds { bounds: WidgetBounds },
    CollapseRenderer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowManagementPlan {
    pub display_state: WindowDisplayState,
    pub bounds: WidgetBounds,
    pub commands: Vec<WindowManagementCommand>,
}

impl WorkArea {
    /// Creates a desktop work area used for widget placement.
    ///
    /// # Example
    ///
    /// ```
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 2.0);
    /// assert_eq!(area.width, 1440);
    /// ```
    pub fn new(x: i32, y: i32, width: u32, height: u32, scale_factor: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
            scale_factor,
        }
    }
}

impl WidgetBounds {
    /// Returns bounds moved by a logical drag delta and clamped to the work area.
    ///
    /// # Example
    ///
    /// ```
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 500, 400, 1.0);
    /// let bounds = lumvise_frontend_core::WidgetBounds { x: 10, y: 10, width: 100, height: 100 };
    /// assert_eq!(bounds.moved_by(20.0, 5.0, area).x, 30);
    /// ```
    pub fn moved_by(self, delta_x: f64, delta_y: f64, work_area: WorkArea) -> Self {
        let next = Self {
            x: offset_coordinate(self.x, delta_x, work_area.scale_factor),
            y: offset_coordinate(self.y, delta_y, work_area.scale_factor),
            ..self
        };
        clamp_widget_bounds(next, work_area)
    }
}

impl WindowLayout {
    /// Creates the initial compact Tauri widget layout.
    ///
    /// # Example
    ///
    /// ```
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// let layout = lumvise_frontend_core::WindowLayout::compact(area);
    /// assert_eq!(layout.surface_mode, lumvise_frontend_core::SurfaceMode::Compact);
    /// ```
    pub fn compact(work_area: WorkArea) -> Self {
        Self {
            work_area,
            active_bounds: widget_bounds_for_mode(work_area, SurfaceMode::Compact),
            surface_mode: SurfaceMode::Compact,
        }
    }

    /// Returns the layout after switching to another surface mode.
    ///
    /// # Example
    ///
    /// ```
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// let layout = lumvise_frontend_core::WindowLayout::compact(area)
    ///     .with_surface_mode(lumvise_frontend_core::SurfaceMode::Shell);
    /// assert_eq!(layout.active_bounds.width, 1180);
    /// ```
    pub fn with_surface_mode(self, surface_mode: SurfaceMode) -> Self {
        Self {
            active_bounds: widget_bounds_for_mode(self.work_area, surface_mode),
            surface_mode,
            ..self
        }
    }

    /// Returns the same surface mode laid out in another desktop work area.
    ///
    /// # Example
    ///
    /// ```
    /// let first = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// let second = lumvise_frontend_core::WorkArea::new(1440, 0, 1440, 900, 1.0);
    /// let layout = lumvise_frontend_core::WindowLayout::compact(first).with_work_area(second);
    /// assert_eq!(layout.active_bounds.x, 1440 + 1440 - 135 - 20);
    /// ```
    pub fn with_work_area(self, work_area: WorkArea) -> Self {
        Self {
            work_area,
            active_bounds: widget_bounds_for_mode(work_area, self.surface_mode),
            ..self
        }
    }

    /// Returns the host commands a Tauri shell should apply for this layout.
    ///
    /// # Example
    ///
    /// ```
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// let plan = lumvise_frontend_core::WindowLayout::compact(area).management_plan();
    /// assert_eq!(plan.display_state, lumvise_frontend_core::WindowDisplayState::CompactOrb);
    /// ```
    pub fn management_plan(self) -> WindowManagementPlan {
        window_management_plan_for_mode(self.work_area, self.surface_mode)
    }
}

/// Calculates the Tauri window bounds for a frontend surface mode.
///
/// # Example
///
/// ```
/// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
/// let bounds = lumvise_frontend_core::widget_bounds_for_mode(area, lumvise_frontend_core::SurfaceMode::Fullscreen);
/// assert_eq!(bounds.width, 1440);
/// ```
pub fn widget_bounds_for_mode(work_area: WorkArea, surface_mode: SurfaceMode) -> WidgetBounds {
    match surface_mode {
        SurfaceMode::Compact => compact_bounds(work_area),
        SurfaceMode::Shell => centered_shell_bounds(work_area),
        SurfaceMode::Fullscreen => fullscreen_bounds(work_area),
    }
}

/// Calculates the physical Settings window bounds for one active monitor.
///
/// Settings targets 1040×760 logical pixels. The target is converted to physical
/// pixels using the monitor scale factor, clamped to the monitor work area, and
/// centered within that work area.
pub fn settings_window_bounds(work_area: WorkArea) -> WidgetBounds {
    let width = logical_to_physical(1040, work_area.scale_factor).min(work_area.width);
    let height = logical_to_physical(760, work_area.scale_factor).min(work_area.height);
    let offset_x = u32_to_i32(work_area.width.saturating_sub(width) / 2);
    let offset_y = u32_to_i32(work_area.height.saturating_sub(height) / 2);
    WidgetBounds {
        x: work_area.x.saturating_add(offset_x),
        y: work_area.y.saturating_add(offset_y),
        width,
        height,
    }
}

/// Chooses compact orb bounds when an expanded window collapses.
///
/// # Example
///
/// ```
/// let first = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
/// let second = lumvise_frontend_core::WorkArea::new(1440, 0, 1440, 900, 1.0);
/// let remembered = lumvise_frontend_core::widget_bounds_for_mode(first, lumvise_frontend_core::SurfaceMode::Compact);
/// let bounds = lumvise_frontend_core::compact_bounds_for_collapse(second, Some(remembered));
/// assert!(bounds.x >= second.x);
/// ```
pub fn compact_bounds_for_collapse(
    current_work_area: WorkArea,
    remembered_bounds: Option<WidgetBounds>,
) -> WidgetBounds {
    if let Some(bounds) = remembered_bounds
        && widget_bounds_fit_work_area(bounds, current_work_area)
    {
        return bounds;
    }
    widget_bounds_for_mode(current_work_area, SurfaceMode::Compact)
}

/// Selects the monitor work area containing the widget center.
///
/// # Example
///
/// ```
/// let first = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
/// let second = lumvise_frontend_core::WorkArea::new(1440, 0, 1440, 900, 1.0);
/// let bounds = lumvise_frontend_core::WidgetBounds { x: 1600, y: 100, width: 135, height: 135 };
/// assert_eq!(lumvise_frontend_core::work_area_for_bounds(bounds, [first, second], first), second);
/// ```
pub fn work_area_for_bounds<I>(bounds: WidgetBounds, work_areas: I, fallback: WorkArea) -> WorkArea
where
    I: IntoIterator<Item = WorkArea>,
{
    let center_x = bounds.x.saturating_add((bounds.width / 2) as i32);
    let center_y = bounds.y.saturating_add((bounds.height / 2) as i32);
    work_area_for_point(center_x, center_y, work_areas, fallback)
}

/// Selects the monitor work area containing a desktop point.
///
/// # Example
///
/// ```
/// let first = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
/// let second = lumvise_frontend_core::WorkArea::new(1440, 0, 1440, 900, 1.0);
/// assert_eq!(lumvise_frontend_core::work_area_for_point(1600, 100, [first, second], first), second);
/// ```
pub fn work_area_for_point<I>(x: i32, y: i32, work_areas: I, fallback: WorkArea) -> WorkArea
where
    I: IntoIterator<Item = WorkArea>,
{
    work_areas
        .into_iter()
        .find(|work_area| work_area_contains_point(*work_area, x, y))
        .unwrap_or(fallback)
}

fn widget_bounds_fit_work_area(bounds: WidgetBounds, work_area: WorkArea) -> bool {
    let bounds_right = i64::from(bounds.x).saturating_add(i64::from(bounds.width));
    let bounds_bottom = i64::from(bounds.y).saturating_add(i64::from(bounds.height));
    let work_right = i64::from(work_area.x).saturating_add(i64::from(work_area.width));
    let work_bottom = i64::from(work_area.y).saturating_add(i64::from(work_area.height));

    bounds.x >= work_area.x
        && bounds.y >= work_area.y
        && bounds_right <= work_right
        && bounds_bottom <= work_bottom
}

fn work_area_contains_point(work_area: WorkArea, x: i32, y: i32) -> bool {
    let right = i64::from(work_area.x).saturating_add(i64::from(work_area.width));
    let bottom = i64::from(work_area.y).saturating_add(i64::from(work_area.height));
    i64::from(x) >= i64::from(work_area.x)
        && i64::from(y) >= i64::from(work_area.y)
        && i64::from(x) < right
        && i64::from(y) < bottom
}

pub fn window_management_plan_for_mode(
    work_area: WorkArea,
    surface_mode: SurfaceMode,
) -> WindowManagementPlan {
    let bounds = widget_bounds_for_mode(work_area, surface_mode);
    let display_state = match surface_mode {
        SurfaceMode::Compact => WindowDisplayState::CompactOrb,
        SurfaceMode::Shell => WindowDisplayState::DashboardShell,
        SurfaceMode::Fullscreen => WindowDisplayState::DashboardFullscreen,
    };
    let mut commands = vec![WindowManagementCommand::Unminimize];
    match surface_mode {
        SurfaceMode::Compact => {
            commands.push(WindowManagementCommand::Unmaximize);
            commands.push(WindowManagementCommand::SetBounds { bounds });
            commands.push(WindowManagementCommand::RememberCompactBounds { bounds });
        }
        SurfaceMode::Shell => {
            commands.push(WindowManagementCommand::Unmaximize);
            commands.push(WindowManagementCommand::SetBounds { bounds });
        }
        SurfaceMode::Fullscreen => {
            commands.push(WindowManagementCommand::Unmaximize);
            commands.push(WindowManagementCommand::SetBounds { bounds });
        }
    }
    WindowManagementPlan {
        display_state,
        bounds,
        commands,
    }
}

/// Clamps widget bounds so dragging cannot lose the orb off screen.
///
/// # Example
///
/// ```
/// let area = lumvise_frontend_core::WorkArea::new(0, 0, 300, 200, 1.0);
/// let bounds = lumvise_frontend_core::WidgetBounds { x: -50, y: 999, width: 100, height: 100 };
/// assert_eq!(lumvise_frontend_core::clamp_widget_bounds(bounds, area).x, 0);
/// ```
pub fn clamp_widget_bounds(bounds: WidgetBounds, work_area: WorkArea) -> WidgetBounds {
    let max_x = work_area.x + work_area.width.saturating_sub(bounds.width) as i32;
    let max_y = work_area.y + work_area.height.saturating_sub(bounds.height) as i32;
    WidgetBounds {
        x: bounds.x.clamp(work_area.x, max_x),
        y: bounds.y.clamp(work_area.y, max_y),
        ..bounds
    }
}

fn compact_bounds(work_area: WorkArea) -> WidgetBounds {
    let compact_size = logical_to_physical(COMPACT_WIDGET_SIZE, work_area.scale_factor);
    let width = compact_size.min(work_area.width);
    let height = compact_size.min(work_area.height);
    let offset_x = logical_i32_to_physical(COMPACT_OFFSET_X, work_area.scale_factor);
    let offset_y = logical_i32_to_physical(COMPACT_OFFSET_Y, work_area.scale_factor);
    WidgetBounds {
        x: work_area
            .x
            .saturating_add(u32_to_i32(work_area.width))
            .saturating_sub(u32_to_i32(width))
            .saturating_sub(offset_x),
        y: work_area
            .y
            .saturating_add(u32_to_i32(work_area.height))
            .saturating_sub(u32_to_i32(height))
            .saturating_sub(offset_y),
        width,
        height,
    }
}

fn centered_shell_bounds(work_area: WorkArea) -> WidgetBounds {
    let width = logical_to_physical(SHELL_WIDTH, work_area.scale_factor).min(work_area.width);
    let height = logical_to_physical(SHELL_HEIGHT, work_area.scale_factor).min(work_area.height);
    WidgetBounds {
        x: work_area.x + (work_area.width.saturating_sub(width) / 2) as i32,
        y: work_area.y + (work_area.height.saturating_sub(height) / 2) as i32,
        width,
        height,
    }
}

fn fullscreen_bounds(work_area: WorkArea) -> WidgetBounds {
    WidgetBounds {
        x: work_area.x,
        y: work_area.y,
        width: work_area.width,
        height: work_area.height,
    }
}

fn offset_coordinate(value: i32, delta: f64, scale_factor: f64) -> i32 {
    let physical_delta = (delta * scale_factor.max(0.1)).round();
    value.saturating_add(physical_delta as i32)
}

pub fn logical_to_physical(value: u32, scale_factor: f64) -> u32 {
    if !scale_factor.is_finite() || scale_factor <= 0.0 {
        return value;
    }
    ((f64::from(value) * scale_factor).round() as u32).max(1)
}

pub fn logical_i32_to_physical(value: i32, scale_factor: f64) -> i32 {
    if !scale_factor.is_finite() || scale_factor <= 0.0 {
        return value;
    }
    (f64::from(value) * scale_factor).round() as i32
}

fn u32_to_i32(value: u32) -> i32 {
    value.min(i32::MAX as u32) as i32
}
