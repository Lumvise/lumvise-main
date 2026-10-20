//! Stages immutable bodies before publishing their locator and graph references.
//! shortcut: retain immutable revisions for captured locators; add reader-safe GC if history size requires it.

use lumvise_contracts::{KnowledgeDocument, KnowledgeDocumentIdentity};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

use super::{record_format as format, reference_graph};

pub(super) fn write(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
    resources: Vec<Value>,
) -> Result<(), PluginError> {
    let anchor = ensure_root(context, &document.project_root)?;
    let body = format::body_artifact(document, &anchor);
    let relationships = reference_graph::relationships(context, document)?;
    stage_body(context, &body)?;
    publish(
        context,
        &document.project_root,
        json!({
        "semantic_elements":[document_element(document, &body)],"semantic_relationships":relationships,
        "removed_paths":[],"replace_paths":[format::storage_path(&document.document_id)],
        "semantic_artifacts":resources}),
    )
}

fn ensure_root(context: &mut PluginContext<'_>, root: &str) -> Result<String, PluginError> {
    let elements = super::records::scoped_elements(
        context,
        root,
        json!({"Location":{
        "project_root":root,"path":format::ROOT_PATH,"line":1,"include_inactive":false}}),
    )?;
    let roots: Vec<_> = elements
        .iter()
        .filter(|element| element["path"] == format::ROOT_PATH && element["project_root"] == root)
        .collect();
    match roots.as_slice() {
        [] => create_root(context, root),
        [element] if format::is_root(element) => {
            Ok(element["semantic_element_id"].as_str().unwrap().into())
        }
        _ => Err(super::invalid(
            "invalid_document_root",
            &json!(roots).to_string(),
            "one Knowledge-owned document root",
        )),
    }
}

fn create_root(context: &mut PluginContext<'_>, root: &str) -> Result<String, PluginError> {
    let id = format::root_id(root);
    if crate::storage::semantic_element(context, &id)?.is_some() {
        return Err(super::invalid(
            "document_root_conflict",
            &id,
            "an unused document root identity",
        ));
    }
    let element = json!({"semantic_element_id":id,"semantic_source_id":format::source_id(root),
        "semantic_element_type":"virtual_document_root","semantic_element_name":"Project documents",
        "path":format::ROOT_PATH,"parent_element_id":null,"start_line":null,"end_line":null,
        "content_fingerprint":null,"metadata":{"knowledge_document_root":{"schema":1}}});
    publish(
        context,
        root,
        json!({"semantic_elements":[element],"semantic_relationships":[],
        "removed_paths":[],"replace_paths":[format::ROOT_PATH],"semantic_artifacts":[]}),
    )?;
    Ok(id)
}

fn document_element(document: &KnowledgeDocument, body: &Value) -> Value {
    json!({"semantic_element_id":document.semantic_element_id,
        "semantic_source_id":format::source_id(&document.project_root),
        "semantic_element_type":"virtual_document","semantic_element_name":document.title,
        "path":format::storage_path(&document.document_id),"parent_element_id":null,
        "start_line":null,"end_line":null,"content_fingerprint":null,
        "metadata":{"knowledge_document":{"schema":2,"document_id":document.document_id,
            "artifact_id":body["artifact_id"]}}})
}

fn stage_body(context: &mut PluginContext<'_>, body: &Value) -> Result<(), PluginError> {
    let stored = context.host_call(
        "storage.semantic",
        json!({"operation":"artifact",
        "artifact_id":body["artifact_id"]}),
    )?;
    if !stored["artifact"].is_null() {
        return validate_staged(body, &stored["artifact"]);
    }
    store_body(context, body)
}

fn store_body(context: &mut PluginContext<'_>, body: &Value) -> Result<(), PluginError> {
    let output = context.host_call(
        "storage.semantic",
        json!({"operation":"upsert_artifact",
        "artifact":body,"media_type":"text/plain; charset=utf-8"}),
    )?;
    if output["artifact_id"] != body["artifact_id"] {
        return Err(super::invalid(
            "invalid_document_storage",
            &output.to_string(),
            "the staged document artifact identity",
        ));
    }
    Ok(())
}

fn validate_staged(body: &Value, stored: &Value) -> Result<(), PluginError> {
    if [
        "artifact_kind",
        "semantic_element_id",
        "metadata",
        "content",
        "title",
    ]
    .iter()
    .all(|key| body[key] == stored[key])
    {
        return Ok(());
    }
    Err(super::invalid(
        "document_body_conflict",
        &stored["artifact_id"].to_string(),
        "an unused body identity or exactly the same immutable document payload",
    ))
}

fn publish(
    context: &mut PluginContext<'_>,
    root: &str,
    mut batch: Value,
) -> Result<(), PluginError> {
    batch["provider_instance_id"] = json!(format::PROVIDER);
    batch["project_root"] = json!(root);
    batch["semantic_sources"] = json!([{"semantic_source_id":format::source_id(root),"kind":"virtual_documents",
            "name":"Knowledge documents","root_path":null,
            "root_uri":format!("lumvise:documents/{}",hex::encode(root))}]);
    let output = crate::semantic::invoke_public(context, "ingest_index_batch", batch)?;
    if output["accepted"] != true {
        return Err(super::invalid(
            "invalid_document_storage",
            &output.to_string(),
            "an accepted document partition publication",
        ));
    }
    Ok(())
}

pub(super) fn remove(
    context: &mut PluginContext<'_>,
    identity: &KnowledgeDocumentIdentity,
) -> Result<(), PluginError> {
    // Keep the payload behind the inactive locator: generators must remember deliberate deletions.
    publish(
        context,
        &identity.project_root,
        json!({"semantic_elements":[],
        "semantic_relationships":[],"removed_paths":[format::storage_path(&identity.document_id)],
        "replace_paths":[],"semantic_artifacts":[]}),
    )
}
