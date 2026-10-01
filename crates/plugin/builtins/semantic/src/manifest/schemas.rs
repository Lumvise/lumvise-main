//! Existing Semantic wire schemas; kept separate from capability registration.
use serde_json::{Value, json};

pub(super) fn ingest_input() -> Value {
    object(
        &[
            "provider_instance_id",
            "project_root",
            "semantic_sources",
            "semantic_elements",
            "semantic_relationships",
        ],
        json!({
            "provider_instance_id": string(), "project_root": string(),
            "ingestion_job_id": nullable(string()),
            "ingestion_page_index": nullable(integer()),
            "ingestion_page_count": nullable(integer()),
            "replace_paths": array(string()), "removed_paths": array(string()),
            "removed_artifact_ids": array(string()),
            "semantic_sources": array(object(&["semantic_source_id", "kind", "name", "root_uri"], json!({
                "semantic_source_id": string(), "kind": string(), "name": string(),
                "root_path": nullable(string()), "root_uri": string()
            }))),
            "semantic_elements": array(object(&[
                "semantic_source_id", "semantic_element_id", "path", "semantic_element_type",
                "semantic_element_name",
            ], json!({
                "semantic_source_id": string(), "semantic_element_id": string(), "path": string(),
                "semantic_element_type": string(), "semantic_element_name": string(),
                "parent_element_id": nullable(string()),
                "content_fingerprint": nullable(string()), "start_line": nullable(integer()),
                "end_line": nullable(integer()), "metadata": nullable(any())
            }))),
            "semantic_relationships": array(object(&[
                "source_element_id", "relationship_kind", "relationship_label"
            ], json!({
                "source_element_id": string(), "target_element_id": nullable(string()),
                "relationship_kind": string(), "relationship_label": string(),
                "target_label": nullable(string()), "target_locator": nullable(string())
            }))),
            "semantic_artifacts": array(object(&[
                "artifact_id", "semantic_element_id", "artifact_kind", "title"
            ], json!({
                "artifact_id": string(), "semantic_element_id": string(), "artifact_kind": string(),
                "title": string(), "content_ref": nullable(string()), "content": nullable(string()),
                "searchable_text": nullable(string()), "content_size_bytes": nullable(integer()),
                "metadata": nullable(any())
            })))
        }),
    )
}

pub(super) fn ingest_output() -> Value {
    object(
        &[
            "accepted",
            "semantic_sources_upserted",
            "semantic_elements_upserted",
            "semantic_relationships_upserted",
            "semantic_artifacts_upserted",
            "semantic_artifacts_removed",
            "ingestion_job_id",
            "ingestion_status",
            "snapshot_freshness",
        ],
        json!({
            "accepted": boolean(), "semantic_sources_upserted": integer(),
            "semantic_elements_upserted": integer(), "semantic_relationships_upserted": integer(),
            "semantic_artifacts_upserted": integer(), "semantic_artifacts_removed": integer(),
            "ingestion_job_id": nullable(string()),
            "ingestion_status": string(),
            "snapshot_freshness": object(&["status", "latest_completed_snapshot", "active_ingestion_job_id"], json!({
                "status": string(), "latest_completed_snapshot": nullable(any()),
                "active_ingestion_job_id": nullable(string())
            }))
        }),
    )
}

pub(super) fn tree_output() -> Value {
    let node = json!({
        "type": "object", "additionalProperties": false,
        "required": ["element", "children"],
        "properties": {"element": {"$ref": "#/$defs/element"},
            "children": {"type": "array", "items": {"$ref": "#/$defs/node"}}}
    });
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$defs": {"element": element_schema(), "node": node},
        "type": "object", "additionalProperties": false,
        "required": ["project_root", "semantic_element_id", "total_nodes", "max_depth",
            "roots", "commit_version", "published_at"],
        "properties": {
            "project_root": string(), "semantic_element_id": nullable(string()),
            "total_nodes": integer(), "max_depth": integer(),
            "commit_version": integer(), "published_at": string(),
            "roots": {"type": "array", "items": {"$ref": "#/$defs/node"}}
        }
    })
}

pub(super) fn dependency_output() -> Value {
    let branch = json!({
        "type": "object", "additionalProperties": false,
        "required": ["relationship", "node", "cycle"],
        "properties": {"relationship": relationship_view_schema(),
            "node": {"$ref": "#/$defs/node"}, "cycle": boolean()}
    });
    let node = json!({
        "type": "object", "additionalProperties": false,
        "required": ["element", "dependencies", "dependents"],
        "properties": {"element": {"$ref": "#/$defs/element"},
            "dependencies": {"type": "array", "items": {"$ref": "#/$defs/branch"}},
            "dependents": {"type": "array", "items": {"$ref": "#/$defs/branch"}}}
    });
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$defs": {"element": element_schema(), "branch": branch, "node": node},
        "type": "object", "additionalProperties": false,
        "required": ["project_root", "semantic_element_id", "direction", "total_nodes",
            "max_depth", "commit_version", "published_at", "root"],
        "properties": {"project_root": string(), "semantic_element_id": string(),
            "direction": string(), "total_nodes": integer(), "max_depth": integer(),
            "commit_version": integer(), "published_at": string(),
            "root": {"$ref": "#/$defs/node"}}
    })
}

pub(super) fn graph_output() -> Value {
    object(
        &[
            "providerId",
            "providerName",
            "dashboardUrl",
            "projectRoot",
            "targetPath",
            "granularity",
            "source",
            "generatedAt",
            "summary",
            "nodes",
            "edges",
            "commitVersion",
            "publishedAt",
        ],
        json!({
            "providerId": nullable(string()), "providerName": nullable(string()),
            "dashboardUrl": nullable(string()), "projectRoot": nullable(string()),
            "targetPath": nullable(string()), "granularity": string(),
            "source": {"type": "string", "enum": ["app-owned-index"]},
            "generatedAt": string(), "commitVersion": integer(), "publishedAt": string(),
            "summary": string(),
            "nodes": array(graph_node_schema()), "edges": array(graph_edge_schema())
        }),
    )
}

pub(super) fn semantic_context_output() -> Value {
    object(
        &[
            "record_kind",
            "elements",
            "relationships",
            "artifacts",
            "commit_version",
            "published_at",
        ],
        json!({
            "record_kind": {"type": "string", "enum": ["elements", "relationships", "artifacts"]},
            "elements": array(full_element_schema()),
            "relationships": array(full_relationship_schema()),
            "artifacts": array(full_artifact_schema()),
            "commit_version": integer(),
            "published_at": string()
        }),
    )
}

pub(super) fn full_element_schema() -> Value {
    object(
        &[
            "project_root",
            "semantic_element_id",
            "semantic_source_id",
            "path",
            "element_kind",
            "name",
            "parent_element_id",
            "content_fingerprint",
            "start_line",
            "end_line",
            "lifecycle",
            "metadata",
        ],
        json!({
            "project_root": string(), "semantic_element_id": string(), "semantic_source_id": string(),
            "path": string(), "element_kind": string(), "name": string(),
            "parent_element_id": nullable(string()), "content_fingerprint": nullable(string()),
            "start_line": nullable(integer()), "end_line": nullable(integer()),
            "lifecycle": string(), "metadata": any()
        }),
    )
}

pub(super) fn full_relationship_schema() -> Value {
    object(
        &[
            "project_root",
            "source_element_id",
            "target_element_id",
            "relationship_kind",
            "label",
            "lifecycle",
            "metadata",
        ],
        json!({
            "project_root": string(), "source_element_id": string(), "target_element_id": string(),
            "relationship_kind": string(), "label": string(), "lifecycle": string(), "metadata": any()
        }),
    )
}

pub(super) fn full_artifact_schema() -> Value {
    object(
        &[
            "project_root",
            "artifact_id",
            "semantic_element_id",
            "artifact_kind",
            "title",
            "content_ref",
            "content",
            "searchable_text",
            "content_size_bytes",
            "metadata",
        ],
        json!({
            "project_root": string(), "artifact_id": string(), "semantic_element_id": string(),
            "artifact_kind": string(), "title": string(), "content_ref": nullable(string()),
            "content": nullable(string()), "searchable_text": nullable(string()),
            "content_size_bytes": nullable(integer()), "metadata": any()
        }),
    )
}

pub(super) fn element_schema() -> Value {
    object(
        &[
            "semantic_element_id",
            "element_kind",
            "name",
            "path",
            "parent_element_id",
            "start_line",
            "end_line",
        ],
        json!({
            "semantic_element_id": string(), "element_kind": string(), "name": string(), "path": string(),
            "parent_element_id": nullable(string()), "start_line": nullable(integer()), "end_line": nullable(integer())
        }),
    )
}

pub(super) fn relationship_view_schema() -> Value {
    object(
        &[
            "source_element_id",
            "target_element_id",
            "relationship_kind",
            "label",
            "target_label",
            "target_locator",
        ],
        json!({
            "source_element_id": string(), "target_element_id": string(), "relationship_kind": string(),
            "label": string(), "target_label": nullable(string()), "target_locator": nullable(string())
        }),
    )
}

pub(super) fn index_log_schema() -> Value {
    object(
        &[
            "provider_instance_id",
            "semantic_source_id",
            "plugin_id",
            "project_root",
            "index_log_id",
            "status",
            "metrics",
            "content_fingerprint",
            "error",
            "started_at",
            "completed_at",
            "created_at",
            "updated_at",
        ],
        json!({
            "provider_instance_id": string(), "semantic_source_id": nullable(string()),
            "plugin_id": nullable(string()), "project_root": nullable(string()),
            "index_log_id": string(), "status": string(), "metrics": any(),
            "content_fingerprint": nullable(string()), "error": nullable(string()),
            "started_at": nullable(string()), "completed_at": nullable(string()),
            "created_at": string(), "updated_at": string()
        }),
    )
}

pub(super) fn provider_schema() -> Value {
    object(
        &[
            "id",
            "name",
            "displayName",
            "projectRoot",
            "dashboardUrl",
            "semanticGraphUrl",
            "registeredAt",
            "lastSeenAt",
            "status",
            "providerInstanceIds",
        ],
        json!({
            "id": string(), "name": string(), "displayName": nullable(string()), "projectRoot": string(),
            "dashboardUrl": nullable(string()), "semanticGraphUrl": nullable(string()),
            "registeredAt": string(), "lastSeenAt": string(), "status": nullable(string()),
            "providerInstanceIds": array(string())
        }),
    )
}

pub(super) fn graph_node_schema() -> Value {
    object(
        &[
            "id",
            "label",
            "kind",
            "parentId",
            "parentLabel",
            "depth",
            "isContainer",
            "path",
            "lineStart",
            "lineEnd",
            "codeSize",
            "connectionStrength",
            "semanticArtifactCount",
            "commentCount",
            "dataSemanticArtifactCount",
            "summary",
            "artifacts",
            "stableRef",
        ],
        json!({
            "id": string(), "label": string(), "kind": string(), "parentId": nullable(string()),
            "parentLabel": nullable(string()), "depth": nullable(integer()), "isContainer": nullable(boolean()),
            "path": nullable(string()), "lineStart": nullable(integer()), "lineEnd": nullable(integer()),
            "codeSize": integer(), "connectionStrength": integer(), "semanticArtifactCount": integer(),
            "commentCount": integer(), "dataSemanticArtifactCount": integer(), "summary": nullable(string()),
            "artifacts": array(graph_artifact_schema()), "stableRef": string()
        }),
    )
}

pub(super) fn graph_artifact_schema() -> Value {
    object(
        &[
            "artifactId",
            "artifactKind",
            "title",
            "text",
            "contentRef",
            "contentSizeBytes",
        ],
        json!({
            "artifactId": string(), "artifactKind": string(), "title": string(),
            "text": nullable(string()), "contentRef": nullable(string()),
            "contentSizeBytes": nullable(integer())
        }),
    )
}

pub(super) fn snapshot_output() -> Value {
    object(
        &[
            "operation_id",
            "project_id",
            "project_root",
            "status",
            "result",
            "failure",
        ],
        json!({
            "operation_id": string(), "project_id": nullable(string()),
            "project_root": string(),
            "status": {"type": "string", "enum": ["queued", "running", "succeeded", "failed", "cancelled"]},
            "result": nullable(any()), "failure": nullable(any())
        }),
    )
}

pub(super) fn graph_edge_schema() -> Value {
    object(
        &[
            "id",
            "source",
            "target",
            "relationship_label",
            "relationship_kind",
            "linkType",
            "weight",
            "samples",
        ],
        json!({
            "id": string(), "source": string(), "target": string(), "relationship_label": string(),
            "relationship_kind": nullable(string()), "linkType": nullable(string()),
            "weight": integer(), "samples": array(string())
        }),
    )
}

pub(super) fn object(required: &[&str], properties: Value) -> Value {
    json!({
        "type": "object", "additionalProperties": false,
        "required": required, "properties": properties
    })
}

pub(super) fn array(items: Value) -> Value {
    json!({"type": "array", "items": items})
}
pub(super) fn string() -> Value {
    json!({"type": "string"})
}
pub(super) fn integer() -> Value {
    json!({"type": "integer"})
}
pub(super) fn number() -> Value {
    json!({"type": "number"})
}
pub(super) fn boolean() -> Value {
    json!({"type": "boolean"})
}
pub(super) fn any() -> Value {
    json!({})
}
pub(super) fn nullable(schema: Value) -> Value {
    json!({"anyOf": [schema, {"type": "null"}]})
}
