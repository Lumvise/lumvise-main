//! Ready-only HTTP and View catalog for independently compiled plugins.
//!
//! Callers may list signed View descriptors and resolve HTTP requests. Package
//! metadata, collision rules, path matching, and process invocation stay here.

#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
thread_local! {
    static SURFACE_LOAD_COUNT: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_surface_load_count() {
    SURFACE_LOAD_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn surface_load_count() -> usize {
    SURFACE_LOAD_COUNT.with(Cell::get)
}

use lumvise_frontend_core::{
    RendererViewDescriptor, RendererViewMenuPlacement, RendererViewSource, ViewRegistry,
};
use lumvise_plugin_package::{
    ExportSurface, HttpMethod, HttpStreamMode, SseStreamPolicy, ViewMenuPlacement,
};
use lumvise_plugin_protocol::WireOutcome;
use lumvise_plugin_runtime::{
    PluginInvocationError, PluginInvocationFailureKind, PluginSystem, PublishedPlugin,
};
use serde_json::{Value, json};

use crate::app::mcp_http::{HttpRequest, HttpResponse, error_response, json_response};
use crate::{AppCore, AppCoreError, Result};

pub(crate) use super::compiled_sse::{CompiledSseRoute, write_compiled_sse};
use super::compiled_views;
use super::invocation::surface_invocation_request;

#[derive(Clone, Debug)]
struct CompiledHttpRoute {
    plugin_id: String,
    export_id: String,
    method: HttpMethod,
    path_template: String,
    stream_mode: HttpStreamMode,
    sse_policy: Option<SseStreamPolicy>,
}

/// One ready-only snapshot of generic compiled plugin surfaces.
pub(crate) struct CompiledSurfaceCatalog {
    http_routes: Vec<CompiledHttpRoute>,
    views: Vec<RendererViewDescriptor>,
    cataloged_plugin_ids: BTreeSet<String>,
}

pub(crate) enum HttpRouteResolution {
    Matched {
        plugin_id: String,
        export_id: String,
        input: Value,
        stream_mode: HttpStreamMode,
        sse_policy: Option<SseStreamPolicy>,
        head_request: bool,
    },
    MethodNotAllowed,
    NotFound,
}

impl CompiledSurfaceCatalog {
    pub(crate) fn load(app: &AppCore) -> Result<Self> {
        #[cfg(test)]
        SURFACE_LOAD_COUNT.with(|count| count.set(count.get() + 1));
        let system = app.plugin_system();
        let mut cataloged = system.cataloged_plugins()?;
        cataloged.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
        let mut published = system.published_plugins()?;
        published.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
        let cataloged_plugin_ids = app.cataloged_production_plugin_ids()?.into_iter().collect();
        let mut http_routes = Vec::new();
        let mut views = Vec::new();
        for plugin in &cataloged {
            append_plugin_http_routes(plugin, &mut http_routes);
        }
        for plugin in &published {
            append_plugin_views(plugin, &mut views);
        }
        http_routes.sort_by(http_route_order);
        views.sort_by(|left, right| {
            left.view_id
                .cmp(&right.view_id)
                .then_with(|| left.plugin_id.cmp(&right.plugin_id))
        });
        reject_http_collisions(&http_routes)?;
        reject_view_collisions(&views)?;
        Ok(Self {
            http_routes,
            views,
            cataloged_plugin_ids,
        })
    }

    pub(crate) fn views(&self) -> &[RendererViewDescriptor] {
        &self.views
    }

    pub(crate) fn merge_views(&self, registry: &mut ViewRegistry) -> Result<()> {
        for plugin_id in &self.cataloged_plugin_ids {
            registry.unregister_plugin(plugin_id);
        }
        for view in &self.views {
            if let Some(existing) = registry.get(&view.view_id)
                && existing.plugin_id != view.plugin_id
            {
                let mut plugin_ids = vec![existing.plugin_id.clone(), view.plugin_id.clone()];
                plugin_ids.sort();
                plugin_ids.dedup();
                return Err(AppCoreError::PluginViewCollision {
                    view_id: view.view_id.clone(),
                    plugin_ids,
                });
            }
            registry.register(view.clone());
        }
        Ok(())
    }

    pub(crate) fn resolve_http(&self, request: &HttpRequest) -> Result<HttpRouteResolution> {
        let path_matches = self
            .http_routes
            .iter()
            .filter_map(|route| match_path(&route.path_template, &request.path).map(|p| (route, p)))
            .collect::<Vec<_>>();
        let Some((route, path_parameters)) = path_matches
            .iter()
            .find(|(route, _)| method_name(route.method) == request.method)
        else {
            return Ok(if path_matches.is_empty() {
                HttpRouteResolution::NotFound
            } else {
                HttpRouteResolution::MethodNotAllowed
            });
        };
        Ok(HttpRouteResolution::Matched {
            plugin_id: route.plugin_id.clone(),
            export_id: route.export_id.clone(),
            input: request_input(request, path_parameters),
            stream_mode: route.stream_mode,
            sse_policy: route.sse_policy.clone(),
            head_request: route.method == HttpMethod::Head,
        })
    }
}

pub(crate) fn invoke_http_route(
    system: &PluginSystem,
    resolution: HttpRouteResolution,
) -> HttpResponse {
    let HttpRouteResolution::Matched {
        plugin_id,
        export_id,
        input,
        stream_mode,
        sse_policy,
        head_request,
    } = resolution
    else {
        return error_response("500 Internal Server Error", "unmatched compiled HTTP route");
    };
    if stream_mode == HttpStreamMode::ServerSentEvents {
        let Some(policy) = sse_policy else {
            return error_response("500 Internal Server Error", "missing signed SSE policy");
        };
        return HttpResponse::compiled_sse(CompiledSseRoute {
            plugin_id,
            export_id,
            input,
            policy,
        });
    }
    let request = surface_invocation_request(
        &plugin_id,
        &export_id,
        input,
        "compiled-http",
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    );
    match system.invoke_controlled(request) {
        Ok(WireOutcome::Succeeded { value }) => success_response(value, stream_mode, head_request),
        Ok(WireOutcome::Failed { error }) => json_response(
            "502 Bad Gateway",
            json!({"error": {
                "code": error.code,
                "message": error.message,
                "details": error.details,
                "retryable": error.retryable,
            }}),
        ),
        Err(error) => invocation_error_response(error),
    }
}

fn invocation_error_response(error: PluginInvocationError) -> HttpResponse {
    let (status, code) = match error.kind() {
        PluginInvocationFailureKind::Busy => ("503 Service Unavailable", "plugin_busy"),
        PluginInvocationFailureKind::DeadlineExceeded => {
            ("504 Gateway Timeout", "plugin_deadline_exceeded")
        }
        PluginInvocationFailureKind::Cancelled => ("499 Client Closed Request", "plugin_cancelled"),
        PluginInvocationFailureKind::Unavailable => {
            ("503 Service Unavailable", "plugin_temporarily_unavailable")
        }
        PluginInvocationFailureKind::InvalidInput => ("400 Bad Request", "plugin_invalid_input"),
        PluginInvocationFailureKind::PluginFailure => ("502 Bad Gateway", "plugin_failure"),
        PluginInvocationFailureKind::Internal => ("500 Internal Server Error", "plugin_internal"),
    };
    json_response(
        status,
        json!({"error": {
            "code": code, "message": error.to_string(), "retryable": error.retryable()
        }}),
    )
}

fn append_plugin_http_routes(plugin: &PublishedPlugin, routes: &mut Vec<CompiledHttpRoute>) {
    for export in &plugin.exports {
        if let ExportSurface::HttpRoute {
            method,
            path_template,
            stream_mode,
            sse_policy,
        } = &export.surface
        {
            routes.push(CompiledHttpRoute {
                plugin_id: plugin.plugin_id.clone(),
                export_id: export.id.clone(),
                method: *method,
                path_template: path_template.clone(),
                stream_mode: *stream_mode,
                sse_policy: sse_policy.clone(),
            });
        }
    }
}

fn append_plugin_views(plugin: &PublishedPlugin, views: &mut Vec<RendererViewDescriptor>) {
    for export in &plugin.exports {
        match &export.surface {
            ExportSurface::HttpRoute { .. } => {}
            ExportSurface::View {
                view_id,
                surface,
                asset_path,
                content_security_policy,
                allowed_host_apis,
                menu_placement,
            } => views.push(RendererViewDescriptor {
                view_id: view_id.clone(),
                plugin_id: plugin.plugin_id.clone(),
                display_name: export.name.clone(),
                surface: compiled_views::renderer_surface(*surface),
                menu_placement: menu_placement.map(renderer_menu_placement),
                source: RendererViewSource::Compiled {
                    asset_url: compiled_views::view_asset_url(&plugin.plugin_id, view_id),
                    asset_path: asset_path.clone(),
                    content_security_policy: content_security_policy.clone(),
                    allowed_host_apis: allowed_host_apis.clone(),
                },
            }),
            _ => {}
        }
    }
}

fn renderer_menu_placement(placement: ViewMenuPlacement) -> RendererViewMenuPlacement {
    match placement {
        ViewMenuPlacement::DesktopSettings => RendererViewMenuPlacement::DesktopSettings,
    }
}

fn reject_http_collisions(routes: &[CompiledHttpRoute]) -> Result<()> {
    for (index, left) in routes.iter().enumerate() {
        for right in routes.iter().skip(index + 1) {
            if left.method == right.method && templates_overlap(left, right) {
                return Err(AppCoreError::PluginHttpCollision {
                    method: method_name(left.method).into(),
                    route_owners: sorted_route_owners(left, right),
                });
            }
        }
    }
    Ok(())
}

fn reject_view_collisions(views: &[RendererViewDescriptor]) -> Result<()> {
    for pair in views.windows(2) {
        if pair[0].view_id == pair[1].view_id {
            let mut plugin_ids = vec![pair[0].plugin_id.clone(), pair[1].plugin_id.clone()];
            plugin_ids.sort();
            plugin_ids.dedup();
            return Err(AppCoreError::PluginViewCollision {
                view_id: pair[0].view_id.clone(),
                plugin_ids,
            });
        }
    }
    Ok(())
}

fn sorted_route_owners(left: &CompiledHttpRoute, right: &CompiledHttpRoute) -> Vec<String> {
    let mut owners = vec![
        format!(
            "{}:{}:{}",
            left.plugin_id, left.export_id, left.path_template
        ),
        format!(
            "{}:{}:{}",
            right.plugin_id, right.export_id, right.path_template
        ),
    ];
    owners.sort();
    owners
}

fn templates_overlap(left: &CompiledHttpRoute, right: &CompiledHttpRoute) -> bool {
    let left = path_segments(&left.path_template);
    let right = path_segments(&right.path_template);
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left == &right || is_parameter(left) || is_parameter(right))
}

fn match_path(template: &str, path: &str) -> Option<BTreeMap<String, String>> {
    let template = path_segments(template);
    let actual = path_segments(path);
    if template.len() != actual.len() {
        return None;
    }
    let mut parameters = BTreeMap::new();
    for (expected, actual) in template.into_iter().zip(actual) {
        if let Some(parameter) = parameter_name(expected) {
            parameters.insert(parameter.to_owned(), actual.to_owned());
        } else if expected != actual {
            return None;
        }
    }
    Some(parameters)
}

fn request_input(request: &HttpRequest, path_parameters: &BTreeMap<String, String>) -> Value {
    json!({
        "method": request.method,
        "path": request.path,
        "path_parameters": path_parameters,
        "query": request.query,
        "body": request_body(&request.body),
        "body_size_bytes": request.body.len(),
    })
}

fn request_body(body: &[u8]) -> Value {
    if body.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(body).unwrap_or_else(|_| match std::str::from_utf8(body) {
        Ok(text) => Value::String(text.to_owned()),
        Err(_) => Value::Array(body.iter().copied().map(Value::from).collect()),
    })
}

fn success_response(value: Value, stream_mode: HttpStreamMode, head_request: bool) -> HttpResponse {
    let (content_type, mut body) = match stream_mode {
        HttpStreamMode::Buffered => (
            "application/json".to_owned(),
            serde_json::to_vec(&value).unwrap_or_default(),
        ),
        HttpStreamMode::ServerSentEvents => (
            "text/event-stream".to_owned(),
            format!("data: {}\n\n", value).into_bytes(),
        ),
    };
    if head_request {
        body.clear();
    }
    HttpResponse::buffered("200 OK", content_type, body, Vec::new())
}

fn http_route_order(left: &CompiledHttpRoute, right: &CompiledHttpRoute) -> std::cmp::Ordering {
    method_name(left.method)
        .cmp(method_name(right.method))
        .then_with(|| left.path_template.cmp(&right.path_template))
        .then_with(|| left.plugin_id.cmp(&right.plugin_id))
        .then_with(|| left.export_id.cmp(&right.export_id))
}

fn path_segments(path: &str) -> Vec<&str> {
    path.trim_start_matches('/').split('/').collect()
}

fn is_parameter(segment: &str) -> bool {
    parameter_name(segment).is_some()
}

fn parameter_name(segment: &str) -> Option<&str> {
    segment
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
}

fn method_name(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Head => "HEAD",
        HttpMethod::Options => "OPTIONS",
    }
}

#[cfg(test)]
mod tests {
    use super::{CompiledHttpRoute, match_path, reject_http_collisions, reject_view_collisions};
    use crate::AppCoreError;
    use lumvise_frontend_core::{RendererViewDescriptor, RendererViewSource, RendererViewSurface};
    use lumvise_plugin_package::{HttpMethod, HttpStreamMode};

    #[test]
    fn path_template_extracts_named_segments() {
        let parameters =
            match_path("/api/items/{item_id}", "/api/items/item-7").expect("matching path");

        assert_eq!(
            parameters.get("item_id").map(String::as_str),
            Some("item-7")
        );
    }

    #[test]
    fn overlapping_templates_fail_with_sorted_owners() {
        let routes = vec![
            route("zeta.plugin", "by_id", "/items/{item_id}"),
            route("alpha.plugin", "literal", "/items/special"),
        ];

        let error = reject_http_collisions(&routes).expect_err("ambiguous routes rejected");

        assert!(matches!(
            error,
            AppCoreError::PluginHttpCollision { method, route_owners }
                if method == "GET"
                    && route_owners == vec![
                        "alpha.plugin:literal:/items/special",
                        "zeta.plugin:by_id:/items/{item_id}",
                    ]
        ));
    }

    #[test]
    fn duplicate_views_fail_with_sorted_plugin_ids() {
        let views = vec![
            view("shared", "zeta.plugin"),
            view("shared", "alpha.plugin"),
        ];

        let error = reject_view_collisions(&views).expect_err("duplicate View rejected");

        assert!(matches!(
            error,
            AppCoreError::PluginViewCollision { view_id, plugin_ids }
                if view_id == "shared"
                    && plugin_ids == vec!["alpha.plugin", "zeta.plugin"]
        ));
    }

    fn route(plugin_id: &str, export_id: &str, path_template: &str) -> CompiledHttpRoute {
        CompiledHttpRoute {
            plugin_id: plugin_id.into(),
            export_id: export_id.into(),
            method: HttpMethod::Get,
            path_template: path_template.into(),
            stream_mode: HttpStreamMode::Buffered,
            sse_policy: None,
        }
    }

    fn view(view_id: &str, plugin_id: &str) -> RendererViewDescriptor {
        RendererViewDescriptor {
            view_id: view_id.into(),
            plugin_id: plugin_id.into(),
            display_name: "Shared".into(),
            surface: RendererViewSurface::DashboardPanel,
            menu_placement: None,
            source: RendererViewSource::Compiled {
                asset_url: "/api/plugin-views/example/shared/asset".into(),
                asset_path: "views/shared.html".into(),
                content_security_policy: "default-src 'none'".into(),
                allowed_host_apis: vec![],
            },
        }
    }
}
