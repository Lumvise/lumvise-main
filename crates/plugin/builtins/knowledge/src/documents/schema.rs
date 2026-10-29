//! Signed document export descriptors. Storage and document rules stay elsewhere.

use serde_json::{Value, json};

pub(crate) const EXPORTS: &[(&str, &str)] = &[
    ("get_document_hook", "Read memory document hook"),
    ("configure_document_hook", "Configure memory document hook"),
    ("resolve_document_space", "Resolve caller document space"),
    ("document.space", "Assistant persistent document space"),
    (
        "resolve_document_reference",
        "Read qualified document reference",
    ),
    ("document.reference", "Assistant qualified reference read"),
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
];

pub(crate) fn description(id: &str) -> Option<&'static str> {
    match id {
        "get_document_hook" => Some(
            "Read the owning Assistant or MCP memory document's maintenance permission and setup state. Ownership comes from the host. Agent-space hooks retain their existing Agent automation interfaces.",
        ),
        "configure_document_hook" => Some(
            "Configure or disable maintenance of an owned Assistant/MCP context or report. Pass the original expected_hook_revision (null for first setup) and stable operation_id. Enabling also requires the original document revision/locator and Agents settings. Exact retries preserve original inputs and intent. Disable requires no source read and fences queued writes immediately. Other workflow ownership is never replaced; human edits hold automatic publication.",
        ),
        "resolve_document_reference" | "document.reference" => Some(
            "Read a source element, Knowledge artifact or document through its authorized project. Selected mode follows that named selection; exact mode never substitutes newer content. The returned reference includes canonical authority and lineage plus the exact observed identity/edit version. Copy it into a qualified document reference to retain evidence on save. A read alone does not pin evidence. Missing and pruned remain explicit; never refresh a saved edit token silently.",
        ),
        "resolve_document_space" | "document.space" => Some(
            "Resolve your host-authorized persistent memory space: MCP callers use purpose current_project; Assistant and agent callers use purpose self. Use the returned project_root with the existing context/report document tools. Memory survives separate conversations and application restarts. Ownership and project restrictions come from the host, never from tool arguments. Unbound caller scopes fail without creating a space.",
        ),
        "create_document" | "document.create" => Some(
            "Create an independent project document without a source-file anchor. Supply a stable document_id, project_root, document_kind, title and content. Use report for Markdown reports, context for reference notes, and canvas for Canvas-owned scene content. Optional references cite sources, artifacts or other documents without giving those targets ownership. For a working artifact citation, copy edit_version from the original get_knowledge or list_knowledge read into its reference; stale or missing edit versions fail without saving.",
        ),
        "get_document" | "document.get" => Some(
            "Read a document by its discovered document_id and exact project_root. The result includes its independent identity, content, grouping path, optional references and current revision. Read before editing; preserve human text and workflow metadata.",
        ),
        "update_document" | "document.update" => Some(
            "Update supplied document fields after a fresh get_document. Pass expected_revision and expected_semantic_element_id from that read; stale revisions fail without writing. Unspecified fields remain. Organization path changes do not move source files. Preserve human edits and references; read back to verify a save. Keep each artifact reference edit_version from the originally read body; never refresh it silently while saving.",
        ),
        "list_documents" | "document.list" => Some(
            "List independent documents in an exact project, optionally filtered by document_kind, query and include_inactive. Without a project, list accessible source projects and exclude private document spaces. Reports do not need source anchors; private spaces require an explicit authorized scope.",
        ),
        "search_documents" | "document.search" => Some(
            "Return up to 10 relevant independent documents ranked by title, content, grouping path and tags, with a query and optional project/kind scope. Inspect returned mode rather than assuming vector retrieval. Results include standalone reports even when no source element is selected. Use list_documents for a complete inventory.",
        ),
        "delete_document" | "document.delete" => Some(
            "Remove the document revision you read in its exact project scope. Pass expected_revision and expected_semantic_element_id from that read; stale deletion fails. Reuse operation_id only for an identical retry. A minimal deletion identity remains; wanted historical content follows retention and pins. Removing a citation target never implicitly deletes another report.",
        ),
        _ => None,
    }
}

pub(super) fn object(required: &[&str], properties: Value) -> Value {
    json!({"type": "object", "required": required, "properties": properties, "additionalProperties": false})
}

fn kind() -> Value {
    json!({"type": "string", "enum": ["report", "canvas", "ruleset", "mission_log", "context"]})
}

pub(super) fn project_identity_schema() -> Value {
    lumvise_contracts::KnowledgeProjectIdentity::input_schema()
}

fn access_schema() -> Value {
    object(
        &["space_id", "required_projects"],
        json!({"space_id":{"type":"string"},
        "required_projects":{"type":"array", "items":project_identity_schema()}}),
    )
}

pub(super) fn space_schema() -> Value {
    object(
        &[
            "space_id",
            "space_kind",
            "project_root",
            "principal_id",
            "security_domain",
            "storage_authority",
            "shared_project",
            "revision",
        ],
        json!({
        "space_id":{"type":"string"}, "space_kind":{"enum":["assistant", "agent", "mcp_project"]},
        "project_root":{"type":"string"}, "principal_id":{"type":"string"},
        "security_domain":{"type":"string"}, "storage_authority":{"type":"string"},
        "shared_project":{"anyOf":[project_identity_schema(), {"type":"null"}]},
        "revision":{"type":"integer", "minimum":1}}),
    )
}

fn reference() -> Value {
    json!({"oneOf": [
        super::citations::qualified_schema(),
        object(&["target_kind", "semantic_element_id"], json!({"target_kind": {"const": "semantic_element"}, "semantic_element_id": {"type": "string"}})),
        object(&["target_kind", "artifact_id"], json!({"target_kind": {"const": "artifact"}, "artifact_id": {"type": "string"},
            "edit_version":{"anyOf":[{"type":"integer","minimum":1},{"type":"null"}]}})),
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
    if let Some(schema) = super::hooks::input_schema(id) {
        return Some(schema);
    }
    if matches!(id, "resolve_document_reference" | "document.reference") {
        return Some(super::citations::read_input());
    }
    if matches!(id, "resolve_document_space" | "document.space") {
        return Some(object(
            &["purpose"],
            json!({"purpose":{"enum":["self", "current_project"]}}),
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
        "get_document" | "document.get" => &["document_id", "project_root"],
        "delete_document" | "document.delete" => &[
            "document_id",
            "project_root",
            "expected_revision",
            "expected_semantic_element_id",
        ],
        "update_document" | "document.update" => &[
            "document_id",
            "project_root",
            "expected_revision",
            "expected_semantic_element_id",
        ],
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
        let mut properties =
            json!({"document_id": {"type": "string"}, "project_root": {"type": "string"}});
        if matches!(id, "delete_document" | "document.delete") {
            properties["expected_revision"] = json!({"type":"integer", "minimum":1});
            properties["expected_semantic_element_id"] = json!({"type":"string", "minLength":1});
            properties["operation_id"] = json!({"type":"string", "minLength":1});
        }
        return properties;
    }
    if matches!(
        id,
        "list_documents" | "document.list" | "search_documents" | "document.search"
    ) {
        return json!({"project_root": {"type": "string"}, "document_kind": kind(), "query": {"type": "string"}, "include_inactive": {"type": "boolean"}});
    }
    let mut properties = fields();
    properties["operation_id"] = json!({"type":"string", "minLength":1});
    if matches!(id, "update_document" | "document.update") {
        properties
            .as_object_mut()
            .expect("document property object")
            .remove("document_kind");
        properties["expected_revision"] = json!({"type": "integer", "minimum": 0});
        properties["expected_semantic_element_id"] = json!({"type":"string", "minLength":1});
        properties["status"] =
            json!({"type": "string", "enum": ["active", "resolved", "superseded", "archived"]});
        properties["superseded_by"] = json!({"type": "string"});
    }
    properties
}

pub(crate) fn record() -> Value {
    let mut properties = fields();
    properties["access"] = access_schema();
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
    if let Some(schema) = super::hooks::output_schema(id) {
        return Some(schema);
    }
    match id {
        "resolve_document_reference" | "document.reference" => {
            Some(super::citations::read_output())
        }
        "resolve_document_space" | "document.space" => {
            Some(object(&["space"], json!({"space":space_schema()})))
        }
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
