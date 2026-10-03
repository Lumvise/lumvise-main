mod guidance;
mod schemas;
pub(crate) use guidance::analysis_description;
use lumvise_plugin_protocol::CURRENT_PROTOCOL_VERSION;
use schemas::*;
use std::collections::BTreeMap;

use lumvise_plugin_package::{
    BackgroundDeliveryPolicy, ExecutionMode, ExportDescriptor, ExportSurface,
    HostCapabilityRequirement, HttpMethod, HttpStreamMode, PluginManifest, ProtocolRange,
    PublisherIdentity,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

/// Stable identity of compiled Semantic.
pub const PLUGIN_ID: &str = "builtin.semantic";
/// Plugin Protocol generation supported by this package.
pub const PACKAGE_PROTOCOL_VERSION: u32 = CURRENT_PROTOCOL_VERSION.major as u32;
/// Command returning packaged Semantic metadata.
pub const MANIFEST_EXPORT_ID: &str = "manifest";
/// MCP export returning containment hierarchy.
pub const TREE_EXPORT_ID: &str = "get_semantic_tree";
/// MCP export returning dependency hierarchy.
pub const DEPENDENCY_TREE_EXPORT_ID: &str = "get_dependency_tree";
/// MCP export resolving one source location.
pub const ELEMENT_AT_LOCATION_EXPORT_ID: &str = "get_element_at_location";
/// MCP export searching semantic elements.
pub const SEARCH_EXPORT_ID: &str = "search_semantic_elements";
/// MCP export rebuilding one bounded page of the Semantic search vector index.
pub const REBUILD_SEARCH_INDEX_EXPORT_ID: &str = "rebuild_search_index";
/// MCP export ingesting one semantic index batch.
pub const INGEST_EXPORT_ID: &str = "ingest_index_batch";
/// MCP export recording index progress.
pub const RECORD_INDEX_LOG_EXPORT_ID: &str = "record_index_log";
/// MCP export reading latest index progress.
pub const LATEST_INDEX_LOG_EXPORT_ID: &str = "latest_index_log";
/// MCP export listing renderer graph providers.
pub const GRAPH_PROVIDERS_EXPORT_ID: &str = "graph_providers";
/// MCP export projecting renderer graph data.
pub const SEMANTIC_GRAPH_EXPORT_ID: &str = "semantic_graph";
/// MCP export returning bounded full-fidelity Semantic records for domain plugins.
pub const SEMANTIC_CONTEXT_EXPORT_ID: &str = "semantic_context";
/// MCP export creating a complete semantic PZ snapshot.
pub const CREATE_SNAPSHOT_EXPORT_ID: &str = "create_semantic_snapshot";
/// MCP export reading one semantic PZ snapshot operation.
pub const SNAPSHOT_STATUS_EXPORT_ID: &str = "semantic_snapshot_status";
/// MCP export cancelling one semantic PZ snapshot operation.
pub const CANCEL_SNAPSHOT_EXPORT_ID: &str = "cancel_semantic_snapshot";
/// HTTP surface for full-fidelity semantic context reads.
pub const HTTP_CONTEXT_EXPORT_ID: &str = "http_semantic_context";
/// HTTP surface for semantic element search.
pub const HTTP_SEARCH_CONTEXT_EXPORT_ID: &str = "http_search_context";
/// HTTP surface for semantic relationship trees.
pub const HTTP_RELATIONSHIP_TREE_EXPORT_ID: &str = "http_semantic_relationship_tree";
/// Storage trigger for semantic element writes.
pub const ELEMENT_TRIGGER_EXPORT_ID: &str = "trigger.semantic_element_upserted";
/// Storage trigger for semantic artifact writes.
pub const ARTIFACT_TRIGGER_EXPORT_ID: &str = "trigger.semantic_artifact_upserted";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Invocation {
    Manifest,
    Tree,
    DependencyTree,
    ElementAtLocation,
    Search,
    RebuildSearchIndex,
    Ingest,
    CreateSnapshot,
    SnapshotStatus,
    CancelSnapshot,
    RecordIndexLog,
    LatestIndexLog,
    GraphProviders,
    SemanticGraph,
    SemanticContext,
    ProjectElementCounts,
    HttpContext,
    HttpSearchContext,
    HttpRelationshipTree,
    StorageTrigger,
}

struct ExportDefinition {
    id: &'static str,
    name: &'static str,
    invocation: Invocation,
}

const EXPORTS: &[ExportDefinition] = &[
    export(
        MANIFEST_EXPORT_ID,
        "Semantic manifest",
        Invocation::Manifest,
    ),
    export(TREE_EXPORT_ID, "Get semantic tree", Invocation::Tree),
    export(
        DEPENDENCY_TREE_EXPORT_ID,
        "Get dependency tree",
        Invocation::DependencyTree,
    ),
    export(
        ELEMENT_AT_LOCATION_EXPORT_ID,
        "Get semantic element at location",
        Invocation::ElementAtLocation,
    ),
    export(
        SEARCH_EXPORT_ID,
        "Search semantic elements",
        Invocation::Search,
    ),
    export(
        REBUILD_SEARCH_INDEX_EXPORT_ID,
        "Rebuild semantic search index",
        Invocation::RebuildSearchIndex,
    ),
    export(
        INGEST_EXPORT_ID,
        "Ingest semantic index batch",
        Invocation::Ingest,
    ),
    export(
        RECORD_INDEX_LOG_EXPORT_ID,
        "Record semantic index log",
        Invocation::RecordIndexLog,
    ),
    export(
        LATEST_INDEX_LOG_EXPORT_ID,
        "Get latest semantic index log",
        Invocation::LatestIndexLog,
    ),
    export(
        GRAPH_PROVIDERS_EXPORT_ID,
        "List semantic graph providers",
        Invocation::GraphProviders,
    ),
    export(
        SEMANTIC_GRAPH_EXPORT_ID,
        "Project semantic graph",
        Invocation::SemanticGraph,
    ),
    export(
        CREATE_SNAPSHOT_EXPORT_ID,
        "Create semantic PZ snapshot",
        Invocation::CreateSnapshot,
    ),
    export(
        SNAPSHOT_STATUS_EXPORT_ID,
        "Get semantic PZ snapshot status",
        Invocation::SnapshotStatus,
    ),
    export(
        CANCEL_SNAPSHOT_EXPORT_ID,
        "Cancel semantic PZ snapshot",
        Invocation::CancelSnapshot,
    ),
    export(
        "project_element_counts",
        "Count project elements",
        Invocation::ProjectElementCounts,
    ),
    export(
        SEMANTIC_CONTEXT_EXPORT_ID,
        "Get full-fidelity semantic context",
        Invocation::SemanticContext,
    ),
    export(
        HTTP_CONTEXT_EXPORT_ID,
        "Semantic context over HTTP",
        Invocation::HttpContext,
    ),
    export(
        HTTP_SEARCH_CONTEXT_EXPORT_ID,
        "Search context over HTTP",
        Invocation::HttpSearchContext,
    ),
    export(
        HTTP_RELATIONSHIP_TREE_EXPORT_ID,
        "Semantic relationship tree over HTTP",
        Invocation::HttpRelationshipTree,
    ),
    export(
        ELEMENT_TRIGGER_EXPORT_ID,
        "Semantic element upserted",
        Invocation::StorageTrigger,
    ),
    export(
        ARTIFACT_TRIGGER_EXPORT_ID,
        "Semantic artifact upserted",
        Invocation::StorageTrigger,
    ),
];

const fn export(id: &'static str, name: &'static str, invocation: Invocation) -> ExportDefinition {
    ExportDefinition {
        id,
        name,
        invocation,
    }
}
fn definition(invocation: Invocation) -> &'static ExportDefinition {
    EXPORTS
        .iter()
        .find(|export| export.invocation == invocation)
        .expect("every Semantic invocation must have one export definition")
}

pub(crate) fn source(target: &str, executable_sha256: &str) -> PluginManifest {
    let executable = format!("bin/lumvise-plugin-semantic-{target}");
    PluginManifest {
        schema_version: 1,
        publisher: PublisherIdentity {
            publisher_id: "lumvise.builtin".into(),
            key_id: "lumvise.release.1".into(),
        },
        plugin_id: PLUGIN_ID.into(),
        plugin_version: env!("CARGO_PKG_VERSION").into(),
        protocol: ProtocolRange {
            min: PACKAGE_PROTOCOL_VERSION,
            max: PACKAGE_PROTOCOL_VERSION,
        },
        targets: BTreeMap::from([(target.into(), executable.clone())]),
        files: BTreeMap::from([(executable, executable_sha256.into())]),
        exports: exports(),
        host_capabilities: vec![
            HostCapabilityRequirement {
                id: "project.source".into(),
                version: "^1.0".into(),
            },
            HostCapabilityRequirement {
                id: "storage.plugin".into(),
                version: "^1.0".into(),
            },
            HostCapabilityRequirement {
                id: "storage.semantic".into(),
                version: "^1.3.0".into(),
            },
            HostCapabilityRequirement {
                id: "semantic.snapshot".into(),
                version: "^1.0".into(),
            },
            HostCapabilityRequirement {
                id: "neural.embed".into(),
                version: "^1.0".into(),
            },
        ],
    }
}

fn exports() -> Vec<ExportDescriptor> {
    let mut exports = vec![
        descriptor(
            Invocation::Manifest,
            ExportSurface::Command,
            object(&[], json!({})),
            object(
                &["plugin_id", "protocol", "exports"],
                json!({
                    "plugin_id": string(), "protocol": integer(), "exports": array(string())
                }),
            ),
        ),
        descriptor(
            Invocation::Tree,
            ExportSurface::McpTool,
            object(
                &[],
                json!({
                    "project_root": nullable(string()), "semantic_element_id": nullable(string()),
                    "max_depth": nullable(integer()), "include_inactive": nullable(boolean())
                }),
            ),
            tree_output(),
        ),
        descriptor(
            Invocation::DependencyTree,
            ExportSurface::McpTool,
            object(
                &["semantic_element_id"],
                json!({
                    "semantic_element_id": string(),
                    "direction": nullable(json!({"type": "string", "enum": ["dependencies", "dependents", "both"]})),
                    "max_depth": nullable(integer()), "include_descendants": nullable(boolean()),
                    "include_inactive": nullable(boolean())
                }),
            ),
            dependency_output(),
        ),
        descriptor(
            Invocation::ElementAtLocation,
            ExportSurface::McpTool,
            object(
                &["project_root", "path", "line"],
                json!({
                    "project_root": string(), "path": string(), "line": {"type": "integer", "minimum": 1},
                    "element_kind": nullable(string()), "include_inactive": nullable(boolean())
                }),
            ),
            object(
                &[
                    "project_root",
                    "path",
                    "line",
                    "semantic_element_id",
                    "element",
                    "candidates",
                    "commit_version",
                    "published_at",
                ],
                json!({
                    "project_root": string(), "path": string(), "line": integer(),
                    "semantic_element_id": nullable(string()), "element": nullable(element_schema()),
                    "candidates": array(element_schema()), "commit_version": integer(),
                    "published_at": string()
                }),
            ),
        ),
        descriptor(
            Invocation::Search,
            ExportSurface::McpTool,
            object(
                &["query"],
                json!({
                    "query": string(), "project_root": nullable(string()), "element_kind": nullable(string()),
                    "include_inactive": nullable(boolean()),
                    "limit": nullable(json!({"type": "integer", "minimum": 1, "maximum": 100}))
                }),
            ),
            object(
                &[
                    "query",
                    "project_root",
                    "mode",
                    "index_state",
                    "candidate_count",
                    "results",
                ],
                json!({
                    "query": string(), "project_root": nullable(string()),
                    "mode": {"type": "string", "enum": ["vector", "lexical_fallback"]},
                    "index_state": {"type": "string", "enum": ["missing", "stale", "rebuilding", "ready", "failed"]},
                    "candidate_count": integer(),
                    "results": array(object(&["element", "score"], json!({
                        "element": element_schema(), "score": number()
                    })))
                }),
            ),
        ),
        descriptor(
            Invocation::RebuildSearchIndex,
            ExportSurface::McpTool,
            object(
                &["project_root"],
                json!({
                    "project_root": string(),
                }),
            ),
            object(
                &[
                    "project_root",
                    "index_state",
                    "indexed_nodes",
                    "commit_version",
                    "published_at",
                ],
                json!({
                    "project_root": string(),
                    "index_state": {"type": "string", "enum": ["rebuilding", "ready"]},
                    "indexed_nodes": integer(), "commit_version": integer(),
                    "published_at": string()
                }),
            ),
        ),
        descriptor(
            Invocation::Ingest,
            ExportSurface::McpTool,
            ingest_input(),
            ingest_output(),
        ),
        descriptor(
            Invocation::RecordIndexLog,
            ExportSurface::McpTool,
            object(
                &["provider_instance_id", "index_log_id", "status"],
                json!({
                    "provider_instance_id": string(), "semantic_source_id": nullable(string()),
                    "plugin_id": nullable(string()), "project_root": nullable(string()),
                    "index_log_id": string(), "status": string(), "metrics": any(),
                    "content_fingerprint": nullable(string()), "error": nullable(string()),
                    "started_at": nullable(string()), "completed_at": nullable(string())
                }),
            ),
            object(&["index_log"], json!({"index_log": index_log_schema()})),
        ),
        descriptor(
            Invocation::LatestIndexLog,
            ExportSurface::McpTool,
            object(
                &[],
                json!({"provider_instance_id": nullable(string()), "project_root": nullable(string())}),
            ),
            object(
                &["index_log"],
                json!({"index_log": nullable(index_log_schema())}),
            ),
        ),
        descriptor(
            Invocation::GraphProviders,
            ExportSurface::McpTool,
            object(&[], json!({})),
            object(
                &["providers"],
                json!({"providers": array(provider_schema())}),
            ),
        ),
        descriptor(
            Invocation::SemanticGraph,
            ExportSurface::McpTool,
            object(
                &["granularity"],
                json!({
                    "providerId": nullable(string()), "projectRoot": nullable(string()),
                    "targetPath": nullable(string()),
                    "granularity": {"type": "string", "enum": ["file", "class", "function", "method", "property"]},
                    "recursive": nullable(boolean()), "includeExternal": nullable(boolean()),
                    "includeFirstNeighbors": nullable(boolean())
                }),
            ),
            graph_output(),
        ),
        descriptor(
            Invocation::CreateSnapshot,
            ExportSurface::McpTool,
            object(&[], json!({"project_root": nullable(string())})),
            snapshot_output(),
        ),
        descriptor(
            Invocation::SnapshotStatus,
            ExportSurface::McpTool,
            object(&["operation_id"], json!({"operation_id": string()})),
            snapshot_output(),
        ),
        descriptor(
            Invocation::CancelSnapshot,
            ExportSurface::McpTool,
            object(&["operation_id"], json!({"operation_id": string()})),
            snapshot_output(),
        ),
        descriptor(
            Invocation::ProjectElementCounts,
            ExportSurface::McpTool,
            object(&["project_root"], json!({"project_root":string()})),
            object(
                &[
                    "commit_version",
                    "published_at",
                    "total_elements",
                    "elements_by_kind",
                ],
                json!({
                    "commit_version":integer(),"published_at":string(),"total_elements":integer(),
                    "elements_by_kind":{"type":"object","additionalProperties":{"type":"integer","minimum":0}}
                }),
            ),
        ),
        descriptor(
            Invocation::SemanticContext,
            ExportSurface::McpTool,
            object(
                &["project_root", "record_kind"],
                json!({
                    "project_root": string(), "root_element_id": nullable(string()),
                    "include_descendants": nullable(boolean()),
                    "record_kind": {"type": "string", "enum": ["elements", "relationships", "artifacts"]}
                }),
            ),
            semantic_context_output(),
        ),
        http_route_descriptor(Invocation::HttpContext, "/api/context"),
        http_route_descriptor(Invocation::HttpSearchContext, "/api/search/context"),
        http_route_descriptor(
            Invocation::HttpRelationshipTree,
            "/api/semantic-relationship-tree",
        ),
    ];
    exports.extend(trigger_exports());
    exports.extend(crate::analysis::manifest::exports());
    exports
}

fn trigger_exports() -> Vec<ExportDescriptor> {
    [
        (
            ELEMENT_TRIGGER_EXPORT_ID,
            "Semantic element upserted",
            vec![
                "semantic.element.upserted".to_string(),
                "semantic.element.deleted".to_string(),
            ],
            "semantic_element",
        ),
        (
            ARTIFACT_TRIGGER_EXPORT_ID,
            "Semantic artifact upserted",
            vec![
                "semantic.artifact.upserted".to_string(),
                "semantic.artifact.deleted".to_string(),
            ],
            "semantic_artifact",
        ),
    ]
    .into_iter()
    .map(|(id, name, event_kinds, entity_kind)| ExportDescriptor {
        description: guidance::description(id).into(),
        id: id.into(),
        name: name.into(),
        surface: ExportSurface::StorageTrigger {
            event_kinds,
            entity_kinds: vec![entity_kind.into()],
            delivery: background_delivery_policy(),
        },
        input_schema: trigger_input_schema(),
        output_schema: trigger_output_schema(),
        admission: None,
        execution: ExecutionMode::Background,
    })
    .collect()
}

fn background_delivery_policy() -> BackgroundDeliveryPolicy {
    BackgroundDeliveryPolicy {
        max_attempts: 5,
        initial_backoff_ms: 250,
        max_backoff_ms: 30_000,
        dead_letter_max_entries: 1_000,
        dead_letter_retention_seconds: 604_800,
    }
}

fn trigger_input_schema() -> Value {
    object(
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
            "changed": array(object(
                &["entity_id", "entity_kind", "disposition"],
                json!({"entity_id": string(), "entity_kind": string(),
                    "disposition": {"type": "string", "enum": ["upserted", "removal"]}})
            ))
        }),
    )
}

fn trigger_output_schema() -> Value {
    object(
        &["acknowledged", "processed"],
        json!({"acknowledged": boolean(), "processed": integer()}),
    )
}

/// Buffered POST route sharing behavior with the matching invocation handler.
fn http_route_descriptor(invocation: Invocation, path: &str) -> ExportDescriptor {
    descriptor(
        invocation,
        ExportSurface::HttpRoute {
            method: HttpMethod::Post,
            path_template: path.into(),
            stream_mode: HttpStreamMode::Buffered,
            sse_policy: None,
        },
        http_envelope_input_schema(),
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object"
        }),
    )
}

fn http_envelope_input_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "required": ["method", "path", "path_parameters", "query", "body", "body_size_bytes"],
        "properties": {
            "method": {"type": "string"},
            "path": {"type": "string"},
            "path_parameters": {"type": "object"},
            "query": {"type": "object"},
            "body": {},
            "body_size_bytes": {"type": "integer"}
        },
        "additionalProperties": false
    })
}

fn descriptor(
    invocation: Invocation,
    surface: ExportSurface,
    input_schema: Value,
    output_schema: Value,
) -> ExportDescriptor {
    let export = definition(invocation);
    ExportDescriptor {
        description: guidance::description(export.id).into(),
        id: export.id.into(),
        name: export.name.into(),
        surface,
        input_schema,
        output_schema,
        admission: None,
        execution: if invocation == Invocation::RebuildSearchIndex {
            ExecutionMode::Background
        } else {
            ExecutionMode::Foreground
        },
    }
}

pub(crate) fn invoke(
    capability_id: &str,
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    if crate::analysis::manifest::supports(capability_id) {
        return crate::analysis::invoke(capability_id, input, context);
    }
    let export = EXPORTS
        .iter()
        .find(|export| export.id == capability_id)
        .ok_or_else(|| PluginError::unknown_capability(capability_id))?;
    invoke_export(export.invocation, input, context)
}

fn invoke_export(
    invocation: Invocation,
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    match invocation {
        Invocation::Manifest => Ok(metadata()),
        Invocation::Tree => crate::query::semantic_tree(input, context),
        Invocation::DependencyTree => crate::query::dependency_tree(input, context),
        Invocation::ElementAtLocation => crate::query::element_at_location(input, context),
        Invocation::Search => crate::query::search(input, context),
        Invocation::RebuildSearchIndex => crate::query::rebuild_search_index(input, context),
        Invocation::Ingest => crate::ingest::ingest(input, context),
        Invocation::RecordIndexLog => crate::ingest::record_log(input, context),
        Invocation::LatestIndexLog => crate::ingest::latest_log(input, context),
        Invocation::GraphProviders => crate::graph::providers(input, context),
        Invocation::SemanticGraph => crate::graph::semantic_graph(input, context),
        Invocation::CreateSnapshot => crate::snapshot::create(input, context),
        Invocation::SnapshotStatus => crate::snapshot::status(input, context),
        Invocation::CancelSnapshot => crate::snapshot::cancel(input, context),
        Invocation::ProjectElementCounts => crate::query::project_element_counts(input, context),
        Invocation::SemanticContext => crate::context::semantic_context(input, context),
        Invocation::HttpContext => crate::http::semantic_context(input, context),
        Invocation::HttpSearchContext => crate::http::search_context(input, context),
        Invocation::HttpRelationshipTree => crate::http::relationship_tree(input, context),
        Invocation::StorageTrigger => crate::triggers::storage_change(input, context),
    }
}

fn metadata() -> Value {
    json!({
        "plugin_id": PLUGIN_ID,
        "protocol": PACKAGE_PROTOCOL_VERSION,
        "exports": exports().into_iter().map(|export| export.id).collect::<Vec<_>>()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_exports_match_manifest_exports() {
        let manifest_ids = exports()
            .into_iter()
            .map(|export| Value::String(export.id))
            .collect::<Vec<_>>();
        let metadata_ids = metadata()["exports"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        assert_eq!(metadata_ids, manifest_ids);
    }

    /// Signed package identity must move in lock-step with the crate version.
    ///
    /// A compiled executable that changes host-protocol behavior (for example
    /// SDK-level handling of `host.host_result`) but keeps `plugin_version`
    /// unchanged is indistinguishable, by identity, from a stale previously
    /// installed build of the same version: the plugin repository's
    /// same-version collision guard then either treats the two as duplicates
    /// or rejects the new build outright, leaving the stale executable
    /// installed. `plugin_version` MUST stay derived from `CARGO_PKG_VERSION`
    /// (never a hardcoded literal) so every source release that warrants a
    /// `Cargo.toml` version bump produces a genuinely new, non-colliding
    /// signed identity.
    #[test]
    fn source_manifest_plugin_version_tracks_crate_version() {
        let manifest = source("test-target", &"a".repeat(64));
        assert_eq!(manifest.plugin_version, env!("CARGO_PKG_VERSION"));
    }
}
