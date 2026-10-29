//! Signed input/output contracts. Invocation and storage rules remain in their owners.

use super::schema::{
    array, artifact_dependency, boolean, closed_object, integer, knowledge_kind, knowledge_status,
    nullable, number, string,
};
use super::transfer_schema;
use super::{ASSISTANT_CREATE_EXPORT_ID, ASSISTANT_UPDATE_EXPORT_ID};
use serde_json::{Value, json};

pub(super) fn input_schema(id: &str) -> Value {
    use crate::*;
    if id == crate::documents::maintenance::EXPORT_ID {
        return crate::documents::maintenance::input_schema();
    }
    if id == crate::documents::inspection::EXPORT_ID {
        return crate::documents::inspection::input_schema();
    }
    if id == crate::documents::delegation::EXPORT_ID {
        return crate::documents::delegation::input_schema();
    }
    if let Some(schema) = crate::publication_journal::input_schema(id) {
        return schema;
    }
    if let Some(schema) = crate::documents::input_schema(id) {
        return if id.starts_with("document.") {
            with_scoped_identity(schema)
        } else {
            schema
        };
    }
    let schema = match id {
        PREVIEW_TRANSFER_EXPORT_ID => id_input("project_root"),
        APPLY_TRANSFER_EXPORT_ID => closed_object(
            &["project_root", "transfer_ids"],
            json!({"project_root": string(), "transfer_ids": array(string()),
                "operation_id": nullable(json!({"type":"string", "minLength":1,
                "description":"Keep this ID when retrying the same reviewed selection; use a new ID after a fresh review."}))}),
        ),
        MANIFEST_EXPORT_ID => closed_object(&[], json!({})),
        PROJECTION_EXPORT_ID => closed_object(
            &["projectRoot", "changesSince"],
            json!({"projectRoot": string(), "changesSince": integer()}),
        ),
        CREATE_EXPORT_ID | ASSISTANT_CREATE_EXPORT_ID => closed_object(
            &[
                "artifact_id",
                "semantic_element_id",
                "knowledge_type",
                "title",
                "content",
            ],
            create_artifact_input_properties(),
        ),
        UPDATE_EXPORT_ID | ASSISTANT_UPDATE_EXPORT_ID => closed_object(
            &["artifact_id", "expected_revision"],
            update_artifact_input_properties(),
        ),
        DELETE_EXPORT_ID => closed_object(
            &["artifact_id", "project_root", "expected_revision"],
            json!({"artifact_id": string(), "project_root": string(), "expected_revision":{"type":"integer","minimum":0}, "operation_id": nullable(json!({"type":"string", "minLength":1,
                "description":"Keep this ID when retrying one removal; use a new ID for a later intentional action."}))}),
        ),
        GET_EXPORT_ID | ASSISTANT_GET_EXPORT_ID => id_input("artifact_id"),
        LIST_DEPENDENTS_EXPORT_ID | ASSISTANT_LIST_DEPENDENTS_EXPORT_ID => closed_object(
            &["target_kind", "target_id"],
            json!({"target_kind": string(), "target_id": string()}),
        ),
        LIST_EXPORT_ID | ASSISTANT_LIST_EXPORT_ID => closed_object(
            &["semantic_element_id"],
            json!({"semantic_element_id": string(), "include_inactive": nullable(boolean())}),
        ),
        LIST_ALL_EXPORT_ID => closed_object(
            &[],
            json!({"semantic_element_ids": nullable(array(string())), "project_root": nullable(string()), "knowledge_type": nullable(knowledge_kind()),
                "tags": nullable(array(string())),
                "limit": nullable(json!({"type": "integer", "minimum": 0, "maximum": 1000})),
                "include_inactive": nullable(boolean())}),
        ),
        SEARCH_EXPORT_ID | ASSISTANT_SEARCH_EXPORT_ID => closed_object(
            &["query"],
            json!({"query": string(), "project_root": nullable(string()),
                "semantic_element_id": nullable(string()), "knowledge_type": nullable(knowledge_kind()),
                "limit": nullable(json!({"type": "integer", "minimum": 0, "maximum": 1000})),
                "include_inactive": nullable(boolean())}),
        ),
        FIND_ELEMENTS_EXPORT_ID | ASSISTANT_FIND_ELEMENTS_EXPORT_ID => closed_object(
            &["query"],
            json!({"query": string(), "project_root": nullable(string()),
                "element_kind": nullable(string()), "include_inactive": nullable(boolean()),
                "limit": nullable(json!({"type": "integer", "minimum": 0, "maximum": 1000}))}),
        ),
        GET_ELEMENT_EXPORT_ID | ASSISTANT_GET_ELEMENT_EXPORT_ID => id_input("semantic_element_id"),
        REBUILD_EXPORT_ID => closed_object(&[], json!({"project_root": nullable(string())})),
        RUN_CULTIVATION_EXPORT_ID => closed_object(
            &["mode", "project_root"],
            json!({"mode": {"type": "string", "enum": [
                "cultivate_from_scratch", "refresh_cultivation", "deepen_abstraction",
                "synthesize_nucleus", "diagnose_on_request"]},
                "project_root": string(), "target_id": nullable(string()),
                "target_path": nullable(string())}),
        ),
        GET_CULTIVATION_RUN_EXPORT_ID => id_input("run_id"),
        ENSURE_C4_EXPORT_ID => c4_input(true),
        DEBUG_C4_EXPORT_ID => c4_input(false),
        ARTIFACT_GENERATION_POLL_EXPORT_ID => closed_object(
            &["delivery_id", "scheduled_at_unix_seconds"],
            json!({"delivery_id": string(), "scheduled_at_unix_seconds": integer()}),
        ),
        _ => closed_object(&[], json!({})),
    };
    if id.starts_with("knowledge.") {
        return with_scoped_identity(schema);
    }
    schema
}

fn with_scoped_identity(mut schema: Value) -> Value {
    let properties = schema
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .expect("Knowledge input schemas must expose object properties");
    // Scoped MCP routing injects all three identity fields into the arguments
    // before the signed schema validates them. `mcp_owner_id` is the owner
    // (e.g. `builtin.assistant`); `plugin_id`/`session_id` identify the lane.
    // They are stripped from the client-visible descriptor in scoped_mcp_rpc.
    properties.insert("mcp_owner_id".into(), string());
    properties.insert("plugin_id".into(), string());
    properties.insert("session_id".into(), string());
    schema
}

pub(super) fn output_schema(id: &str) -> Value {
    if id == crate::documents::maintenance::EXPORT_ID {
        return crate::documents::maintenance::output_schema();
    }
    if id == crate::documents::inspection::EXPORT_ID {
        return crate::documents::inspection::output_schema();
    }
    if id == crate::documents::delegation::EXPORT_ID {
        return crate::documents::delegation::output_schema();
    }
    use crate::*;
    if let Some(schema) = crate::publication_journal::output_schema(id) {
        return schema;
    }
    if let Some(schema) = crate::documents::output_schema(id) {
        return schema;
    }
    match id {
        PREVIEW_TRANSFER_EXPORT_ID => transfer_schema::preview(),
        APPLY_TRANSFER_EXPORT_ID => transfer_schema::applied(),
        MANIFEST_EXPORT_ID => closed_object(
            &["plugin_id", "protocol", "exports"],
            json!({"plugin_id": string(), "protocol": integer(), "exports": array(string())}),
        ),
        PROJECTION_EXPORT_ID => canonical_projection_schema(),
        CREATE_EXPORT_ID
        | ASSISTANT_CREATE_EXPORT_ID
        | UPDATE_EXPORT_ID
        | ASSISTANT_UPDATE_EXPORT_ID
        | GET_EXPORT_ID
        | ASSISTANT_GET_EXPORT_ID => {
            closed_object(&["artifact"], json!({"artifact": artifact_schema()}))
        }
        DELETE_EXPORT_ID => closed_object(
            &["artifact_id", "deleted"],
            json!({"artifact_id": string(), "deleted": boolean()}),
        ),
        LIST_ALL_EXPORT_ID | LIST_EXPORT_ID | ASSISTANT_LIST_EXPORT_ID => closed_object(
            &["artifacts"],
            json!({"artifacts": array(artifact_schema())}),
        ),
        LIST_DEPENDENTS_EXPORT_ID | ASSISTANT_LIST_DEPENDENTS_EXPORT_ID => closed_object(
            &["artifacts"],
            json!({"artifacts": array(artifact_schema())}),
        ),
        SEARCH_EXPORT_ID | ASSISTANT_SEARCH_EXPORT_ID => search_output_schema(),
        FIND_ELEMENTS_EXPORT_ID | ASSISTANT_FIND_ELEMENTS_EXPORT_ID => closed_object(
            &["elements", "mode"],
            json!({"elements": array(element_schema()), "mode": string()}),
        ),
        GET_ELEMENT_EXPORT_ID | ASSISTANT_GET_ELEMENT_EXPORT_ID => {
            closed_object(&["element"], json!({"element": element_schema()}))
        }
        REBUILD_EXPORT_ID => closed_object(&["job"], json!({"job": {"type": "object"}})),
        RUN_CULTIVATION_EXPORT_ID | GET_CULTIVATION_RUN_EXPORT_ID => {
            closed_object(&["run"], json!({"run": cultivation_run_schema()}))
        }
        ENSURE_C4_EXPORT_ID => ensure_c4_output_schema(),
        DEBUG_C4_EXPORT_ID => closed_object(
            &[
                "status",
                "projectRoot",
                "requestedTarget",
                "reportTarget",
                "scope",
                "functionalDocuments",
                "report",
            ],
            json!({"status": {"const": "ok"}, "projectRoot": string(),
                "requestedTarget": {"type": "object"}, "reportTarget": {"type": "object"},
                "scope": {"type": "object"}, "functionalDocuments": array(json!({"type": "object"})),
                "report": {"type": "object"}}),
        ),
        ARTIFACT_GENERATION_POLL_EXPORT_ID => closed_object(
            &["pending", "completed"],
            json!({"pending": integer(), "completed": integer()}),
        ),
        _ => closed_object(&[], json!({})),
    }
}

/// Media anchors live on the field: create guidance has a length cap.
const METADATA_HINT: &str = "Optional. A media artifact may pin anchor_selector: kind image_region with x, y, width, height as 0-1 fractions, or kind audio_segment or video_segment with start_seconds and end_seconds.";

fn create_artifact_input_properties() -> Value {
    json!({
        "operation_id": nullable(json!({"type":"string", "minLength":1,
            "description":"Keep this ID when retrying one action; use a new ID for a later intentional action. Omitted: identical requests replay."})),
        "artifact_id": string(), "semantic_element_id": string(),
        "knowledge_type": knowledge_kind(), "title": string(), "content": string(),
        "tags": array(string()), "dependencies": array(artifact_dependency()),
        "metadata": {"type": "object", "description": METADATA_HINT}, "path": nullable(string()),
        "project_root": string(), "expires_at": nullable(string())
    })
}

fn update_artifact_input_properties() -> Value {
    json!({
        "operation_id": nullable(json!({"type":"string", "minLength":1,
            "description":"Keep this ID when retrying one action; use a new ID for a later intentional action, including after restoring history. Omitted: identical requests replay."})),
        "artifact_id": string(), "semantic_element_id": string(),
        "knowledge_type": nullable(knowledge_kind()), "title": nullable(string()),
        "content": nullable(string()), "tags": nullable(array(string())),
        "dependencies": nullable(array(artifact_dependency())),
        "metadata": nullable(json!({"type": "object"})), "path": nullable(string()),
        "project_root": nullable(string()), "status": nullable(knowledge_status()),
        "superseded_by": nullable(string()), "expires_at": nullable(string()),
        "expected_revision": {"type": "integer", "minimum": 0}
    })
}

pub(crate) fn artifact_schema() -> Value {
    closed_object(
        &[
            "artifact_id",
            "semantic_element_id",
            "knowledge_type",
            "title",
            "content",
            "tags",
            "dependencies",
            "metadata",
            "status",
            "revision",
        ],
        json!({"artifact_id": string(), "semantic_element_id": string(), "knowledge_type": knowledge_kind(),
            "edit_version": nullable(json!({"type":"integer", "minimum":1})),
            "title": string(), "content": string(), "tags": array(string()), "metadata": {},
            "dependencies": array(artifact_dependency()),
            "path": nullable(string()),
            "project_root": nullable(string()),
            "status": knowledge_status(), "revision": integer(),
            "status_changed_at": nullable(string()), "expires_at": nullable(string()),
            "superseded_by": nullable(string()), "created_at": nullable(string()),
            "updated_at": nullable(string())}),
    )
}

pub(crate) fn element_schema() -> Value {
    closed_object(
        &[
            "semantic_element_id",
            "element_kind",
            "name",
            "path",
            "parent_element_id",
            "start_line",
            "end_line",
        ],
        json!({"semantic_element_id": string(), "element_kind": string(), "name": string(),
            "project_root": string(), "semantic_source_id": string(), "content_fingerprint": string(),
            "lifecycle": string(), "metadata": {},
            "path": string(), "parent_element_id": nullable(string()), "start_line": nullable(integer()),
            "end_line": nullable(integer())}),
    )
}

fn search_output_schema() -> Value {
    closed_object(
        &["mode", "results"],
        json!({"mode": {"type": "string", "enum": ["vector", "lexical"]},
            "results": array(closed_object(
                &["artifact_id", "semantic_element_id", "knowledge_type", "title", "status", "score"],
                json!({"artifact_id": string(), "semantic_element_id": string(),
                    "knowledge_type": knowledge_kind(), "title": string(),
                    "status": knowledge_status(), "score": number()}))) }),
    )
}

fn cultivation_run_schema() -> Value {
    closed_object(
        &[
            "run_id",
            "mode",
            "project_root",
            "document_ids",
            "diagnostics_created",
            "updated_at",
        ],
        json!({"run_id": string(), "mode": string(), "project_root": string(),
            "document_ids": array(string()), "diagnostics_created": boolean(), "updated_at": string()}),
    )
}

fn c4_input(mutable: bool) -> Value {
    let mut properties = json!({"project_root": string(), "target_id": nullable(string()),
        "target_path": nullable(string())});
    if mutable {
        properties["refresh"] = nullable(boolean());
        properties["retry_failed"] = nullable(boolean());
        properties["provider_id"] = nullable(string());
        properties["model"] = nullable(string());
    }
    closed_object(&["project_root"], properties)
}

pub(super) fn http_input_schema() -> Value {
    closed_object(
        &[
            "method",
            "path",
            "path_parameters",
            "query",
            "body",
            "body_size_bytes",
        ],
        json!({"method": string(), "path": string(), "path_parameters": {"type": "object"},
            "query": {"type": "object"}, "body": {}, "body_size_bytes": integer()}),
    )
}

pub(super) fn http_stream_input_schema() -> Value {
    let mut schema = http_input_schema();
    schema["properties"]["cursor"] = nullable(string());
    schema["properties"]["max_events"] = json!({"type": "integer", "minimum": 1, "maximum": 100});
    schema
}

pub(super) fn http_output_schema(id: &str) -> Value {
    use crate::*;
    match id {
        HTTP_MANIFEST_EXPORT_ID => closed_object(
            &["generatedAt", "projects"],
            json!({"generatedAt": string(), "projects": array(closed_object(
                &["sourceId", "displayName", "status", "granularities"],
                json!({"sourceId": string(), "displayName": string(), "status": string(),
                    "granularities": array(string())})))}),
        ),
        HTTP_SETUP_EXPORT_ID => closed_object(
            &[
                "schemaVersion",
                "vaultId",
                "origin",
                "mode",
                "createdAt",
                "mcpBaseUrl",
                "projects",
            ],
            json!({"schemaVersion": {"const": 1}, "vaultId": nullable(string()),
                "origin": {"const": "lumvise-mcp"}, "mode": string(), "createdAt": string(),
                "mcpBaseUrl": string(), "projects": array(json!({"type": "object"}))}),
        ),
        HTTP_EXPORT_EXPORT_ID => canonical_projection_schema(),
        HTTP_SYNC_EXPORT_ID => sync_projection_schema(),
        HTTP_PAGE_EXPORT_ID => canonical_projection_element_schema(),
        HTTP_C4_EXPORT_ID => http_c4_output_schema(),
        HTTP_C4_ACTION_EXPORT_ID => closed_object(
            &["action", "method", "path", "query"],
            json!({"action": string(), "method": string(), "path": string(), "query": {"type": "object"}}),
        ),
        HTTP_C4_DEBUG_EXPORT_ID => output_schema(DEBUG_C4_EXPORT_ID),
        HTTP_WRITE_EXPORT_ID => output_schema(CREATE_EXPORT_ID),
        HTTP_CREATE_ARTIFACT_EXPORT_ID => output_schema(CREATE_EXPORT_ID),
        HTTP_UPDATE_ARTIFACT_EXPORT_ID => output_schema(UPDATE_EXPORT_ID),
        HTTP_DELETE_ARTIFACT_EXPORT_ID => output_schema(DELETE_EXPORT_ID),
        HTTP_RESOLVE_TARGET_EXPORT_ID => output_schema(GET_ELEMENT_EXPORT_ID),
        HTTP_EVENTS_EXPORT_ID => stream_page_schema(),
        HTTP_PROJECTION_ARTIFACTS_EXPORT_ID => closed_object(
            &["artifacts"],
            json!({"artifacts": array(artifact_schema())}),
        ),
        _ => closed_object(&[], json!({})),
    }
}

fn ensure_c4_output_schema() -> Value {
    let mut ready = status_branch(
        &["status", "created", "document", "provider_id", "model"],
        &["failed_element_ids", "failure_reason"],
        "ready",
    );
    ready["properties"]["document"] = crate::documents::record_schema();
    json!({"type": "object", "additionalProperties": false,
    "properties": {"status": string(), "created": boolean(), "document": nullable(crate::documents::record_schema()),
        "provider_id": nullable(string()), "model": nullable(string()), "request_id": string(),
        "target_fingerprint": string(), "missing_element_ids": array(string()),
        "pending_element_ids": array(string()), "submitted": integer(), "document_id": string(),
        "target_path": string(), "reason_code": string(), "reason": string(),
        "failed_element_ids": array(string()), "failure_reason": string()},
    "oneOf": [ready,
        status_branch(&["status", "request_id", "target_fingerprint", "missing_element_ids",
            "pending_element_ids", "submitted", "document_id", "target_path"], &["failed_element_ids"], "pending"),
        required_status(&["status", "reason_code", "reason", "target_fingerprint", "document_id", "target_path"], "not_ready"),
        required_status(&["status", "created", "document_id", "document", "reason_code", "reason"], "held")
    ]})
}

fn http_c4_output_schema() -> Value {
    json!({"type": "object", "additionalProperties": false,
    "properties": {"status": string(), "created": boolean(), "documentId": string(),
        "targetPath": string(), "requestId": string(), "targetFingerprint": string(),
        "missingElementIds": array(string()), "pendingElementIds": array(string()),
        "reasonCode": string(), "reason": string(), "failedElementIds": array(string())},
    "oneOf": [
        status_branch(&["status", "created", "documentId", "targetPath"], &["failedElementIds"], "ready"),
        status_branch(&["status", "requestId", "targetFingerprint", "missingElementIds",
            "pendingElementIds", "documentId", "targetPath"], &["failedElementIds"], "pending"),
        required_status(&["status", "reasonCode", "reason", "targetFingerprint", "documentId", "targetPath"], "not_ready"),
        required_status(&["status", "created", "reasonCode", "reason", "documentId"], "held")
    ]})
}

fn required_status(required: &[&str], status: &str) -> Value {
    status_branch(required, &[], status)
}

/// One closed `oneOf` branch keyed by `status`; `optional` names may appear.
fn status_branch(required: &[&str], optional: &[&str], status: &str) -> Value {
    let mut properties = serde_json::Map::new();
    for name in required.iter().chain(optional) {
        properties.insert((*name).to_owned(), json!({}));
    }
    properties.insert("status".into(), json!({"const": status}));
    json!({"type": "object", "additionalProperties": false,
        "required": required, "properties": properties})
}

fn sync_projection_schema() -> Value {
    closed_object(
        &[
            "schemaVersion",
            "sourceId",
            "baseRevision",
            "targetRevision",
            "fullReset",
            "generatedAt",
            "syncToken",
            "spaces",
            "pageHashes",
            "changedPages",
            "deletedPageIds",
        ],
        json!({"schemaVersion": {"const": 1}, "sourceId": string(),
            "baseRevision": nullable(integer()), "targetRevision": integer(),
            "fullReset": boolean(), "generatedAt": string(), "syncToken": string(),
            "spaces": canonical_spaces_schema(), "pageHashes": string_map(),
            "changedPages": array(canonical_projection_element_schema()),
            "deletedPageIds": array(string())}),
    )
}

fn canonical_projection_schema() -> Value {
    closed_object(
        &[
            "projectRoot",
            "generatedAt",
            "syncToken",
            "spaces",
            "elements",
        ],
        json!({
            "projectRoot": string(), "generatedAt": string(), "syncToken": string(),
            "spaces": canonical_spaces_schema(),
            "elements": array(canonical_projection_element_schema())
        }),
    )
}

fn canonical_spaces_schema() -> Value {
    array(closed_object(
        &["spaceId", "title", "status", "writePolicy"],
        json!({"spaceId": string(), "title": string(), "status": string(),
            "writePolicy": string()}),
    ))
}

fn string_map() -> Value {
    json!({"type": "object", "additionalProperties": {"type": "string"}})
}

fn canonical_projection_element_schema() -> Value {
    closed_object(
        &[
            "elementId",
            "space",
            "kind",
            "title",
            "markdown",
            "contentMd5",
            "pathHint",
            "sourceId",
            "sourceRefs",
            "syncToken",
            "changeMarker",
            "ownership",
            "writePolicy",
            "children",
            "artifacts",
            "linkTargets",
            "properties",
        ],
        json!({"elementId": string(), "space": string(), "kind": string(), "title": string(),
            "markdown": string(), "contentMd5": string(), "pathHint": string(), "sourceId": string(),
            "sourceRefs": array(json!({"type": "object"})), "syncToken": string(),
            "changeMarker": string(), "ownership": {"type": "object"}, "writePolicy": string(),
            "children": array(json!({"type": "object"})), "artifacts": array(json!({"type": "object"})),
            "linkTargets": array(closed_object(
                &["elementId", "title", "notePathHint", "heading"],
                json!({"elementId": string(), "title": string(), "notePathHint": string(),
                    "heading": nullable(string())}),
            )),
            "properties": {"type": "object"}}),
    )
}

pub(super) fn trigger_input_schema() -> Value {
    closed_object(
        &[
            "project_root",
            "base_revision",
            "target_revision",
            "changed",
        ],
        json!({
            "project_root": string(),
            "base_revision": {"type": "integer"},
            "target_revision": {"type": "integer"},
            "changed": array(closed_object(
                &["entity_id", "entity_kind", "disposition"],
                json!({"entity_id": string(), "entity_kind": string(),
                    "disposition": {"type": "string", "enum": ["upserted", "removal"]}})
            ))
        }),
    )
}

pub(super) fn trigger_output_schema() -> Value {
    closed_object(
        &["acknowledged", "events"],
        json!({"acknowledged": {"type": "boolean"}, "events": {"type": "integer"}}),
    )
}

fn stream_page_schema() -> Value {
    closed_object(
        &["events", "next_cursor", "done"],
        json!({
            "events": array(closed_object(
                &["id", "event", "data"],
                json!({"id": string(), "event": string(), "data": {},
                    "retry_ms": json!({"type": "integer", "minimum": 0})})
            )),
            "next_cursor": nullable(string()), "done": boolean()
        }),
    )
}

fn id_input(field: &str) -> Value {
    closed_object(&[field], json!({(field): string()}))
}
