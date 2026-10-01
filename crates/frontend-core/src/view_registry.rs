//! Dynamic view registry: lets plugins register renderer views
//! (whiteboard, semantic graph, ...) without the base knowing their internals.

use crate::state::FrontendCore;
use serde::{Deserialize, Serialize};

/// Surface placement requested inside the dashboard shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RendererViewSurface {
    /// Separate native window owned by a ready signed plugin.
    NativeWindow,
    /// Inline dashboard panel.
    DashboardPanel,
    /// Floating dashboard layer.
    Overlay,
    /// Full dashboard work area.
    Fullscreen,
}

/// Native menu placement for opening a ready plugin View.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RendererViewMenuPlacement {
    /// Desktop settings menu.
    DesktopSettings,
}

/// Host loading contract for one renderer view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RendererViewSource {
    /// Sandboxed asset and permissions copied from a verified package manifest.
    Compiled {
        /// Ready-only host URL serving verified signed bytes.
        asset_url: String,
        /// Canonical package-relative signed entry asset.
        asset_path: String,
        /// Content Security Policy applied to the sandboxed renderer.
        content_security_policy: String,
        /// Explicit host APIs available inside the sandbox.
        allowed_host_apis: Vec<String>,
    },
}

/// Renderer-owned description of one dynamically registered view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RendererViewDescriptor {
    /// Stable host-wide view identity.
    pub view_id: String,
    /// Plugin owning the view.
    pub plugin_id: String,
    /// Human label shown by the dashboard shell.
    pub display_name: String,
    /// Requested dashboard placement.
    pub surface: RendererViewSurface,
    /// Optional host menu placement copied from the signed package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menu_placement: Option<RendererViewMenuPlacement>,
    /// Loading and isolation policy.
    pub source: RendererViewSource,
}

/// Ownership map for dashboard views. Base owns voice/playback/broadcasts;
/// plugins register whiteboard/semantic_graph/etc. here at startup.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewRegistry {
    descriptors: Vec<RendererViewDescriptor>,
}

impl ViewRegistry {
    /// Registers a plugin view, replacing any prior descriptor with the same id.
    ///
    /// # Example
    /// use lumvise_frontend_core::{RendererViewDescriptor, RendererViewSource, RendererViewSurface, ViewRegistry};
    /// let mut registry = ViewRegistry::default();
    /// let descriptor = RendererViewDescriptor {
    ///     view_id: "semantic_graph".to_string(),
    ///     plugin_id: "builtin.knowledge".to_string(),
    ///     display_name: "Semantic Graph".to_string(),
    ///     surface: RendererViewSurface::Fullscreen,
    ///     menu_placement: None,
    ///     source: RendererViewSource::Compiled {
    ///         asset_url: "/api/plugin-views/builtin.knowledge/semantic_graph/assets/".into(),
    ///         asset_path: "views/semantic_graph/index.html".into(),
    ///         content_security_policy: "default-src 'self'".into(),
    ///         allowed_host_apis: vec![],
    ///     },
    /// };
    /// registry.register(descriptor);
    /// assert_eq!(registry.list().len(), 1);
    pub fn register(&mut self, descriptor: RendererViewDescriptor) {
        self.descriptors
            .retain(|existing| existing.view_id != descriptor.view_id);
        self.descriptors.push(descriptor);
    }

    /// Removes all views owned by a plugin (called on plugin teardown).
    ///
    /// # Example
    /// use lumvise_frontend_core::{RendererViewDescriptor, RendererViewSource, RendererViewSurface, ViewRegistry};
    /// let mut registry = ViewRegistry::default();
    /// registry.register(RendererViewDescriptor {
    ///     view_id: "whiteboard".into(), plugin_id: "builtin.assistant".into(),
    ///     display_name: "Whiteboard".into(), surface: RendererViewSurface::Fullscreen,
    ///     source: RendererViewSource::Compiled {
    ///         asset_url: "/api/plugin-views/builtin.assistant/whiteboard/assets/".into(),
    ///         asset_path: "views/whiteboard/index.html".into(),
    ///         content_security_policy: "default-src 'self'".into(),
    ///         allowed_host_apis: vec![],
    ///     },
    /// });
    /// registry.unregister_plugin("builtin.assistant");
    /// assert!(registry.list().is_empty());
    pub fn unregister_plugin(&mut self, plugin_id: &str) {
        self.descriptors
            .retain(|descriptor| descriptor.plugin_id != plugin_id);
    }

    /// Removes a single view by id.
    pub fn unregister_view(&mut self, view_id: &str) {
        self.descriptors
            .retain(|descriptor| descriptor.view_id != view_id);
    }

    /// All currently registered view descriptors.
    pub fn list(&self) -> &[RendererViewDescriptor] {
        &self.descriptors
    }

    /// Descriptor for a view id, if registered.
    pub fn get(&self, view_id: &str) -> Option<&RendererViewDescriptor> {
        self.descriptors
            .iter()
            .find(|descriptor| descriptor.view_id == view_id)
    }

    /// Plugin id owning a view id, if any.
    pub fn owner_of(&self, view_id: &str) -> Option<&str> {
        self.get(view_id)
            .map(|descriptor| descriptor.plugin_id.as_str())
    }
}

impl FrontendCore {
    /// Registers a plugin-owned renderer view (whiteboard, semantic graph, ...).
    /// Called by app-core during plugin startup wiring; no app spawn required.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_frontend_core::{FrontendCore, RendererViewDescriptor, RendererViewSource, RendererViewSurface};
    /// let mut core = FrontendCore::default();
    /// core.register_view(RendererViewDescriptor {
    ///     view_id: "whiteboard".to_string(),
    ///     plugin_id: "builtin.assistant".to_string(),
    ///     display_name: "Whiteboard".to_string(),
    ///     surface: RendererViewSurface::Fullscreen,
    ///     menu_placement: None,
    ///     source: RendererViewSource::Compiled {
    ///         asset_url: "/api/plugin-views/builtin.assistant/whiteboard/assets/".into(),
    ///         asset_path: "views/whiteboard/index.html".into(),
    ///         content_security_policy: "default-src 'self'".into(),
    ///         allowed_host_apis: vec![],
    ///     },
    /// });
    /// assert_eq!(core.list_views().len(), 1);
    /// ```
    pub fn register_view(&mut self, descriptor: RendererViewDescriptor) {
        self.state.views.register(descriptor);
    }

    /// Removes all views owned by a plugin (called on plugin teardown).
    pub fn unregister_plugin_views(&mut self, plugin_id: &str) {
        self.state.views.unregister_plugin(plugin_id);
    }

    /// All currently registered view descriptors.
    pub fn list_views(&self) -> &[RendererViewDescriptor] {
        self.state.views.list()
    }
}
