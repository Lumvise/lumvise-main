use crate::AppCore;
use crate::app::mcp_http::{HttpRequest, HttpResponse, error_response};

use super::compiled_surfaces::{CompiledSurfaceCatalog, HttpRouteResolution, invoke_http_route};

pub(crate) fn plugin_http_response(app: &AppCore, request: &HttpRequest) -> Option<HttpResponse> {
    if let Some(response) = super::mcp_http_bridge::plugin_mcp_bridge_response(app, request) {
        return Some(response);
    }
    if let Some(response) = super::compiled_views::view_asset_response(app.plugin_system(), request)
    {
        return Some(response);
    }
    let compiled = match CompiledSurfaceCatalog::load(app) {
        Ok(catalog) => catalog,
        Err(error) => return Some(error_response("500 Internal Server Error", error)),
    };
    match compiled.resolve_http(request) {
        Ok(resolution @ HttpRouteResolution::Matched { .. }) => {
            return Some(invoke_http_route(app.plugin_system(), resolution));
        }
        Ok(HttpRouteResolution::MethodNotAllowed) => {
            return Some(error_response(
                "405 Method Not Allowed",
                format!(
                    "method {:?} is unsupported for compiled plugin path {:?}; expected signed route method",
                    request.method, request.path
                ),
            ));
        }
        Ok(HttpRouteResolution::NotFound) => {}
        Err(error) => return Some(error_response("500 Internal Server Error", error)),
    }
    None
}
