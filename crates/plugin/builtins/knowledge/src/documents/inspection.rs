//! Native inspection is a scoped adapter over the existing document exports, not another store.
use lumvise_contracts::{KnowledgeSpaceDescriptor, KnowledgeSpaceInspection};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) const EXPORT_ID: &str = "inspect_document_space";
const DOCUMENT_EXPORTS: &[&str] = &[
    "get_document_hook",
    "configure_document_hook",
    "create_document",
    "get_document",
    "update_document",
    "delete_document",
    "list_documents",
    "search_documents",
    "resolve_document_reference",
    "get_document_write_receipt",
    "get_document_citation_retention",
    "reconcile_document_citations",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectionRequest {
    selection: KnowledgeSpaceInspection,
    export_id: String,
    input: Value,
}

#[derive(Deserialize)]
struct InspectedSpace {
    space: KnowledgeSpaceDescriptor,
    writable: bool,
}

pub(crate) fn invoke(
    input: Value,
    context: &mut PluginContext<'_>,
    vectors: &crate::search::KnowledgeSearchVectors,
) -> Result<Value, PluginError> {
    let request: InspectionRequest = crate::parse(input, "native memory inspection request")?;
    if request.export_id != "open" && !DOCUMENT_EXPORTS.contains(&request.export_id.as_str()) {
        return Err(super::invalid(
            "invalid_inspection_export",
            &request.export_id,
            "a document inspection operation",
        ));
    }
    let binding = context.host_call(
        "runtime.knowledge_scope",
        json!({"operation":"inspect_space","selection":request.selection}),
    )?;
    let inspected: InspectedSpace = crate::parse(binding, "host-authorized human inspection")?;
    let output = inspect(request, &inspected, context, vectors)?;
    Ok(json!({"space":inspected.space,"writable":inspected.writable,"output":output}))
}

fn inspect(
    request: InspectionRequest,
    inspected: &InspectedSpace,
    context: &mut PluginContext<'_>,
    vectors: &crate::search::KnowledgeSearchVectors,
) -> Result<Value, PluginError> {
    let mut input = request.input.as_object().cloned().ok_or_else(|| {
        super::invalid(
            "invalid_inspection_input",
            &request.input.to_string(),
            "a document request object",
        )
    })?;
    let root = &inspected.space.project_root;
    if request.export_id == "resolve_document_reference" {
        return super::invoke(&request.export_id, json!(input), context, vectors);
    }
    if input
        .get("project_root")
        .is_some_and(|value| value.as_str() != Some(root))
    {
        return Err(super::invalid(
            "inspection_space_changed",
            &input["project_root"].to_string(),
            "the original host-selected memory space",
        ));
    }
    if request.export_id == "open" {
        if inspected.writable {
            super::publication::ensure_space(context, &inspected.space)?;
        }
        return Ok(json!({}));
    }
    input.insert("project_root".into(), json!(root));
    if crate::publication_journal::EXPORTS
        .iter()
        .any(|(id, _)| *id == request.export_id)
    {
        return crate::publication_journal::invoke(&request.export_id, json!(input), context);
    }
    super::invoke(&request.export_id, json!(input), context, vectors)
}

pub(crate) fn input_schema() -> Value {
    let selection = json!({"oneOf":[
        super::schema::object(&["space_kind"], json!({"space_kind":{"const":"assistant"}})),
        super::schema::object(&["space_kind","agent_id"], json!({"space_kind":{"const":"agent"},"agent_id":{"type":"string","minLength":1}})),
        super::schema::object(&["space_kind","project_root"], json!({"space_kind":{"const":"mcp_project"},"project_root":{"type":"string","minLength":1}}))]});
    let mut variants = vec![inspection_schema(
        &selection,
        "open",
        super::schema::object(&[], json!({})),
    )];
    for export in DOCUMENT_EXPORTS {
        let mut schema = super::input_schema(export)
            .or_else(|| crate::publication_journal::input_schema(export))
            .expect("document schema");
        if export != &"resolve_document_reference" {
            if let Some(required) = schema["required"].as_array_mut() {
                required.retain(|name| name != "project_root");
            }
        }
        variants.push(inspection_schema(&selection, export, schema));
    }
    let mut schema = super::schema::object(
        &["selection", "export_id", "input"],
        json!({
            "selection":selection, "export_id":{"type":"string"}, "input":{"type":"object"}
        }),
    );
    schema["oneOf"] = json!(variants);
    schema
}

pub(crate) fn output_schema() -> Value {
    super::schema::object(
        &["space", "writable", "output"],
        json!({"space":super::schema::space_schema(),
        "writable":{"type":"boolean"},"output":{"type":"object"}}),
    )
}

fn inspection_schema(selection: &Value, export: &str, input: Value) -> Value {
    super::schema::object(
        &["selection", "export_id", "input"],
        json!({
        "selection":selection,"export_id":{"const":export},"input":input}),
    )
}
