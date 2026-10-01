//! Signed View asset routing and renderer-neutral surface mapping.

use lumvise_frontend_core::RendererViewSurface;
use lumvise_plugin_package::ViewSurface;
use lumvise_plugin_runtime::{PluginRuntimeError, PluginSystem};

use crate::app::mcp_http::{HttpRequest, HttpResponse, error_response};

pub(crate) fn view_asset_response(
    system: &PluginSystem,
    request: &HttpRequest,
) -> Option<HttpResponse> {
    let (plugin_id, view_id, relative_path) = parse_asset_path(&request.path)?;
    if request.method != "GET" {
        return Some(error_response(
            "405 Method Not Allowed",
            "compiled View assets require GET",
        ));
    }
    Some(
        match system.read_view_asset(&plugin_id, &view_id, &relative_path) {
            Ok(asset) => verified_asset_response(asset),
            Err(
                PluginRuntimeError::NotReady(_)
                | PluginRuntimeError::NotInstalled(_)
                | PluginRuntimeError::ViewNotFound { .. },
            ) => error_response("404 Not Found", "compiled View asset unavailable"),
            Err(error) => error_response("409 Conflict", error),
        },
    )
}

fn verified_asset_response(asset: lumvise_plugin_runtime::PluginViewAsset) -> HttpResponse {
    HttpResponse::buffered(
        "200 OK",
        asset_content_type(&asset.asset_path),
        asset.bytes,
        vec![
            (
                "Content-Security-Policy".into(),
                asset.content_security_policy,
            ),
            ("X-Content-Type-Options".into(), "nosniff".into()),
            ("Cache-Control".into(), "no-store".into()),
        ],
    )
}

fn parse_asset_path(path: &str) -> Option<(String, String, String)> {
    let suffix = path.strip_prefix("/api/plugin-views/")?;
    let mut segments = suffix.split('/');
    let plugin_id = decode_segment(segments.next()?)?;
    let view_id = decode_segment(segments.next()?)?;
    if segments.next()? != "assets" {
        return None;
    }
    let remaining = segments.collect::<Vec<_>>();
    let relative_path = if remaining.is_empty() || remaining == [""] {
        String::new()
    } else {
        remaining
            .into_iter()
            .map(decode_segment)
            .collect::<Option<Vec<_>>>()?
            .join("/")
    };
    Some((plugin_id, view_id, relative_path))
}

fn decode_segment(segment: &str) -> Option<String> {
    if segment == "." || segment == ".." || segment.contains('\\') {
        return None;
    }
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let high = *bytes.get(index + 1)?;
        let low = *bytes.get(index + 2)?;
        decoded.push((hex_nibble(high)? << 4) | hex_nibble(low)?);
        index += 3;
    }
    let decoded = String::from_utf8(decoded).ok()?;
    (!decoded.is_empty()
        && decoded != "."
        && decoded != ".."
        && !decoded.contains(['/', '\\', '\0']))
    .then_some(decoded)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn asset_content_type(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

pub(crate) fn renderer_surface(surface: ViewSurface) -> RendererViewSurface {
    match surface {
        ViewSurface::NativeWindow => RendererViewSurface::NativeWindow,
        ViewSurface::DashboardPanel => RendererViewSurface::DashboardPanel,
        ViewSurface::Overlay => RendererViewSurface::Overlay,
        ViewSurface::Fullscreen => RendererViewSurface::Fullscreen,
    }
}

pub(crate) fn view_asset_url(plugin_id: &str, view_id: &str) -> String {
    format!("/api/plugin-views/{plugin_id}/{view_id}/assets/")
}
