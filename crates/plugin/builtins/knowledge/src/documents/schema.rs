//! Signed document export descriptors. Storage and document rules stay elsewhere.

use serde_json::{Value, json};

pub(crate) const EXPORTS: &[(&str, &str)] = &[
    ("create_document", "Create independent document"),
    ("document.create", "Assistant document create"),
    ("get_document", "Read independent document"),
    ("document.get", "Assistant document read"),
    ("update_document", "Update independent document"),
    ("document.update", "Assistant document update"),
    ("list_documents", "List independent documents"),
    ("document.list", "Assistant document list"),
    ("search_documents", "Search document content"),
    ("document.search", "Assistant document search"),
    ("delete_document", "Delete independent document"),
    ("document.delete", "Assistant document delete"),
    (
        "convert_legacy_documents",
        "Convert retired Knowledge records",
    ),
];

pub(crate) fn description(id: &str) -> Option<&'static str> {
    match id {
        "convert_legacy_documents" => Some(
            "Preview or apply the explicit project-scoped conversion of retired Knowledge records into independent documents. Defaults to preview=true. Apply with preview=false only when the document consumers are available. Preserves IDs, human content, lifecycle, references and binary attachments; generic tasks never become scheduled missions. Retries preserve staged document edits; conflicting legacy changes stop retirement.",
        ),
        "create_document" | "document.create" => Some(
            "Create an independent project document without a source-file anchor. Supply a stable document_id, project_root, kind, title and content. Use report for Markdown reports and canvas for Canvas-owned scene content. Optional references cite sources, artifacts or other documents without giving those targets ownership.",
        ),
        "get_document" | "document.get" => Some(
            "Read a document by its discovered document_id and exact project_root. The result includes its independent identity, content, grouping path, optional references and current revision. Read before editing; preserve human text and workflow metadata.",
        ),
        "update_document" | "document.update" => Some(
            "Update supplied document fields after a fresh get_document. Pass expected_revision from that read; stale revisions fail without writing. Unspecified fields remain. Organization path changes do not move source files. Preserve human edits and references; read back to verify a save.",
        ),
        "list_documents" | "document.list" => Some(
            "List independent documents in an exact project, optionally filtered by document_kind, query and include_inactive. Without a project, list accessible non-agent projects. Reports do not need source anchors; private Agent Spaces require an explicit scope and the calling agent's grants.",
        ),
        "search_documents" | "document.search" => Some(
            "Search independent document title, content, grouping path and tags with a query and optional project/kind scope. Inspect returned mode rather than assuming vector retrieval. Results include standalone reports even when no source element is selected.",
        ),
        "delete_document" | "document.delete" => Some(
            "Remove a discovered document within its exact project scope. Read it first and preserve wanted content. Removing a citation target never implicitly deletes another report; existing citations may become broken references.",
        ),
        _ => None,
    }
}

fn object(required: &[&str], properties: Value) -> Value {
    json!({"type": "object", "required": required, "properties": properties, "additionalProperties": false})
}

fn kind() -> Value {
    json!({"type": "string", "enum": ["report", "canvas", "ruleset", "mission_log", "context"]})
}

fn reference() -> Value {
    json!({"oneOf": [
        object(&["target_kind", "semantic_element_id"], json!({"target_kind": {"const": "semantic_element"}, "semantic_element_id": {"type": "string"}})),
        object(&["target_kind", "artifact_id"], json!({"target_kind": {"const": "artifact"}, "artifact_id": {"type": "string"}})),
        object(&["target_kind", "document_id"], json!({"target_kind": {"const": "document"}, "document_id": {"type": "string"}}))
    ]})
}

fn fields() -> Value {
    json!({"document_id": {"type": "string"}, "project_root": {"type": "string"},
        "document_kind": kind(), "title": {"type": "string"}, "content": {"type": "string"},
        "path": {"anyOf": [{"type": "string"}, {"type": "null"}]},
        "tags": {"type": "array", "items": {"type": "string"}},
        "references": {"type": "array", "items": reference()}, "metadata": {}})
}

pub(crate) fn input_schema(id: &str) -> Option<Value> {
    if id == "convert_legacy_documents" {
        return Some(object(
            &["project_root"],
            json!({"project_root": {"type": "string"}, "preview": {"type": "boolean", "default": true}}),
        ));
    }
    let required: &[&str] = match id {
        "create_document" | "document.create" => &[
            "document_id",
            "project_root",
            "document_kind",
            "title",
            "content",
        ],
        "get_document" | "document.get" | "delete_document" | "document.delete" => {
            &["document_id", "project_root"]
        }
        "update_document" | "document.update" => {
            &["document_id", "project_root", "expected_revision"]
        }
        "list_documents" | "document.list" => &[],
        "search_documents" | "document.search" => &["query"],
        _ => return None,
    };
    Some(object(required, input_fields(id)))
}

fn input_fields(id: &str) -> Value {
    if matches!(
        id,
        "get_document" | "document.get" | "delete_document" | "document.delete"
    ) {
        return json!({"document_id": {"type": "string"}, "project_root": {"type": "string"}});
    }
    if matches!(
        id,
        "list_documents" | "document.list" | "search_documents" | "document.search"
    ) {
        return json!({"project_root": {"type": "string"}, "document_kind": kind(), "query": {"type": "string"}, "include_inactive": {"type": "boolean"}});
    }
    let mut properties = fields();
    if matches!(id, "update_document" | "document.update") {
        properties
            .as_object_mut()
            .expect("document property object")
            .remove("document_kind");
        properties["expected_revision"] = json!({"type": "integer", "minimum": 0});
        properties["status"] =
            json!({"type": "string", "enum": ["active", "resolved", "superseded", "archived"]});
        properties["superseded_by"] = json!({"type": "string"});
    }
    properties
}

fn record() -> Value {
    let mut properties = fields();
    properties["semantic_element_id"] = json!({"type": "string"});
    properties["attachment_owner_id"] = json!({"type": "string"});
    properties["revision"] = json!({"type": "integer"});
    properties["status"] = json!({"type": "string"});
    for key in [
        "created_at",
        "updated_at",
        "status_changed_at",
        "superseded_by",
        "expires_at",
    ] {
        properties[key] = json!({"type": "string"});
    }
    object(
        &[
            "document_id",
            "semantic_element_id",
            "project_root",
            "document_kind",
            "title",
            "content",
            "references",
            "tags",
            "metadata",
            "status",
            "revision",
        ],
        properties,
    )
}

pub(crate) fn output_schema(id: &str) -> Option<Value> {
    match id {
        "convert_legacy_documents" => Some(object(
            &["project_root", "preview", "total", "conversions"],
            json!({
                "project_root": {"type": "string"}, "preview": {"type": "boolean"}, "total": {"type": "integer"},
                "conversions": {"type": "array", "items": object(&["artifact_id", "document_id", "document_kind", "already_staged"], json!({
                    "artifact_id": {"type": "string"}, "document_id": {"type": "string"}, "document_kind": kind(), "already_staged": {"type": "boolean"}
                }))}
            }),
        )),
        "create_document" | "document.create" | "get_document" | "document.get"
        | "update_document" | "document.update" => {
            Some(object(&["document"], json!({"document": record()})))
        }
        "list_documents" | "document.list" => Some(object(
            &["documents", "total"],
            json!({"documents": {"type": "array", "items": record()}, "total": {"type": "integer"}}),
        )),
        "search_documents" | "document.search" => Some(object(
            &["documents", "total", "mode", "scores"],
            json!({"documents": {"type": "array", "items": record()}, "total": {"type": "integer"}, "mode": {"type": "string"}, "scores": {"type": "object", "additionalProperties": {"type": "number"}}}),
        )),
        "delete_document" | "document.delete" => Some(object(
            &["document_id", "deleted"],
            json!({"document_id": {"type": "string"}, "deleted": {"type": "boolean"}}),
        )),
        _ => None,
    }
}
