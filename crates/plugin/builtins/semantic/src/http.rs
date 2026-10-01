//! HTTP envelope adapters for the semantic read routes.
//!
//! The daemon delivers signed HTTP-route invocations as an envelope
//! (`method`/`path`/`path_parameters`/`query`/`body`/`body_size_bytes`).
//! These adapters unwrap the JSON body and delegate to the existing
//! invocation handlers, so HTTP and MCP tool surfaces share one behavior.
//! Request shapes are canonical in `lumvise-contracts::semantic::v2`.

use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::Value;

/// `POST /api/context` → full-fidelity project semantic context.
pub(crate) fn semantic_context(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    crate::context::semantic_context(json_body(&input, "semantic context request")?, context)
}

/// `POST /api/search/context` → semantic element search.
pub(crate) fn search_context(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    crate::query::search(json_body(&input, "search context request")?, context)
}

/// `POST /api/semantic-relationship-tree` → dependency/relationship tree.
pub(crate) fn relationship_tree(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    crate::query::dependency_tree(json_body(&input, "relationship tree request")?, context)
}

fn json_body(input: &Value, expected: &str) -> Result<Value, PluginError> {
    let body = input.get("body").cloned().unwrap_or(Value::Null);
    if body.is_object() {
        return Ok(body);
    }
    Err(PluginError::new(
        "semantic_http_body_invalid",
        format!("{expected} requires a JSON object request body"),
        false,
    ))
}
