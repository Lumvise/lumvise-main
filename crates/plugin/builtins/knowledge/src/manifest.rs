use lumvise_plugin_protocol::CURRENT_PROTOCOL_VERSION;
use std::collections::BTreeMap;

use lumvise_plugin_package::{
    BackgroundDeliveryPolicy, ExecutionMode, ExportDescriptor, ExportSurface,
    HostCapabilityRequirement, HttpMethod, HttpStreamMode, PluginManifest, ProtocolRange,
    PublisherIdentity, SseStreamPolicy,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

mod guidance;
mod schema;
mod transfer_schema;

use schema::{
    array, artifact_dependency, boolean, closed_object, integer, knowledge_kind, nullable, number,
    string,
};

/// Stable identity of compiled Knowledge.
pub const PLUGIN_ID: &str = "builtin.knowledge";
/// Plugin Protocol generation supported by the package.
pub const PACKAGE_PROTOCOL_VERSION: u32 = CURRENT_PROTOCOL_VERSION.major as u32;
/// Command returning package metadata.
pub const MANIFEST_EXPORT_ID: &str = "manifest";
/// Command returning the canonical project Knowledge projection.
pub const PROJECTION_EXPORT_ID: &str = "project_knowledge";
/// Creates one Knowledge artifact.
pub const CREATE_EXPORT_ID: &str = "create_knowledge";
/// Updates one Knowledge artifact.
pub const UPDATE_EXPORT_ID: &str = "update_knowledge";
/// Deletes one Knowledge artifact within its project scope.
pub const DELETE_EXPORT_ID: &str = "delete_knowledge";
/// Reads one Knowledge artifact.
pub const GET_EXPORT_ID: &str = "get_knowledge";
/// Lists artifacts owned by one semantic element.
pub const LIST_EXPORT_ID: &str = "list_knowledge_for_element";
/// Previews explicit cross-project artifact transfer candidates without writing.
pub const PREVIEW_TRANSFER_EXPORT_ID: &str = "preview_knowledge_transfer";
/// Copies only selected, currently eligible artifacts to destination elements.
pub const APPLY_TRANSFER_EXPORT_ID: &str = "apply_knowledge_transfer";

/// Lists every Knowledge artifact, optionally filtered.
pub const LIST_ALL_EXPORT_ID: &str = "list_knowledge";

/// Lists Knowledge artifacts that depend on one semantic element or artifact.
pub const LIST_DEPENDENTS_EXPORT_ID: &str = "list_knowledge_dependents";
/// Assistant alias for reverse Knowledge dependency traversal.
pub const ASSISTANT_LIST_DEPENDENTS_EXPORT_ID: &str = "knowledge.list_dependents";
/// Searches Knowledge artifacts.
pub const SEARCH_EXPORT_ID: &str = "search_knowledge";
/// Finds semantic elements through compiled Semantic.
pub const FIND_ELEMENTS_EXPORT_ID: &str = "find_semantic_elements";
/// Reads one semantic element through compiled Semantic.
pub const GET_ELEMENT_EXPORT_ID: &str = "get_semantic_element";
/// Accepts Knowledge vector rebuild work.
pub const REBUILD_EXPORT_ID: &str = "rebuild_knowledge_vectors";
/// Runs Knowledge cultivation.
pub const RUN_CULTIVATION_EXPORT_ID: &str = "run_cultivation";
/// Reads one durable cultivation run.
pub const GET_CULTIVATION_RUN_EXPORT_ID: &str = "get_cultivation_run";
/// Ensures a scoped C4 nucleus.
pub const ENSURE_C4_EXPORT_ID: &str = "ensure_c4_nucleus";
/// Diagnoses one scoped C4 nucleus without mutation.
pub const DEBUG_C4_EXPORT_ID: &str = "debug_c4_nucleus";
/// Recurring consumer for MCP-generated semantic artifacts.
pub const ARTIFACT_GENERATION_POLL_EXPORT_ID: &str = "poll_functional_artifact_generation";
// Workspace voice sessions edit the same artifacts as the renderer. These
// scoped names reuse the existing create/update handlers and storage contract.
const ASSISTANT_CREATE_EXPORT_ID: &str = "knowledge.create";
const ASSISTANT_UPDATE_EXPORT_ID: &str = "knowledge.update";
/// Assistant alias for Knowledge search.
pub const ASSISTANT_SEARCH_EXPORT_ID: &str = "knowledge.search";
/// Assistant alias for element-scoped Knowledge listing.
pub const ASSISTANT_LIST_EXPORT_ID: &str = "knowledge.list_for_element";
/// Assistant alias for reading Knowledge.
pub const ASSISTANT_GET_EXPORT_ID: &str = "knowledge.get";
/// Assistant alias for semantic element search.
pub const ASSISTANT_FIND_ELEMENTS_EXPORT_ID: &str = "knowledge.find_elements";
/// Assistant alias for reading one semantic element.
pub const ASSISTANT_GET_ELEMENT_EXPORT_ID: &str = "knowledge.get_element";
/// Signed HTTP manifest route export.
pub const HTTP_MANIFEST_EXPORT_ID: &str = "http.knowledge.manifest";
/// Signed HTTP setup route export.
pub const HTTP_SETUP_EXPORT_ID: &str = "http.knowledge.setup";
/// Signed HTTP projection export route.
pub const HTTP_EXPORT_EXPORT_ID: &str = "http.knowledge.export";
/// Signed HTTP projection page route.
pub const HTTP_PAGE_EXPORT_ID: &str = "http.knowledge.page";
/// Signed HTTP versioned Obsidian sync route.
pub const HTTP_SYNC_EXPORT_ID: &str = "http.knowledge.sync";
/// Signed HTTP scoped C4 route.
pub const HTTP_C4_EXPORT_ID: &str = "http.knowledge.c4_nucleus";
/// Signed HTTP scoped C4 action descriptor route.
pub const HTTP_C4_ACTION_EXPORT_ID: &str = "http.knowledge.c4_action";
/// Signed HTTP scoped C4 diagnostic route.
pub const HTTP_C4_DEBUG_EXPORT_ID: &str = "http.knowledge.c4_debug";
/// Signed HTTP Knowledge write route.
pub const HTTP_WRITE_EXPORT_ID: &str = "http.knowledge.write";
/// Signed HTTP route creating a Knowledge artifact.
pub const HTTP_CREATE_ARTIFACT_EXPORT_ID: &str = "http.knowledge.artifacts.create";
/// Signed HTTP route updating a Knowledge artifact.
pub const HTTP_UPDATE_ARTIFACT_EXPORT_ID: &str = "http.knowledge.artifacts.update";
/// Signed HTTP route deleting a Knowledge artifact.
pub const HTTP_DELETE_ARTIFACT_EXPORT_ID: &str = "http.knowledge.artifacts.delete";
/// Signed HTTP target-resolution route.
pub const HTTP_RESOLVE_TARGET_EXPORT_ID: &str = "http.knowledge.resolve_target";
/// Signed HTTP live-event route.
pub const HTTP_EVENTS_EXPORT_ID: &str = "http.knowledge.events";
/// Signed HTTP projection-artifact route.
pub const HTTP_PROJECTION_ARTIFACTS_EXPORT_ID: &str = "http.knowledge.projection_artifacts";
/// Storage trigger for semantic graph changes.
pub const ELEMENT_TRIGGER_EXPORT_ID: &str = "trigger.semantic_element_upserted";

const ALL_MCP_EXPORTS: &[(&str, &str)] = &[
    (MANIFEST_EXPORT_ID, "Knowledge manifest"),
    (PREVIEW_TRANSFER_EXPORT_ID, "Preview Knowledge transfer"),
    (
        APPLY_TRANSFER_EXPORT_ID,
        "Apply selected Knowledge transfer",
    ),
    (LIST_DEPENDENTS_EXPORT_ID, "List Knowledge dependents"),
    (CREATE_EXPORT_ID, "Create Knowledge"),
    (ASSISTANT_CREATE_EXPORT_ID, "Assistant Knowledge create"),
    (UPDATE_EXPORT_ID, "Update Knowledge"),
    (ASSISTANT_UPDATE_EXPORT_ID, "Assistant Knowledge update"),
    (DELETE_EXPORT_ID, "Delete Knowledge"),
    (GET_EXPORT_ID, "Get Knowledge"),
    (LIST_EXPORT_ID, "List Knowledge for element"),
    (LIST_ALL_EXPORT_ID, "List Knowledge"),
    (SEARCH_EXPORT_ID, "Search Knowledge"),
    (FIND_ELEMENTS_EXPORT_ID, "Find semantic elements"),
    (GET_ELEMENT_EXPORT_ID, "Get semantic element"),
    (REBUILD_EXPORT_ID, "Rebuild Knowledge vectors"),
    (RUN_CULTIVATION_EXPORT_ID, "Run Knowledge cultivation"),
    (
        GET_CULTIVATION_RUN_EXPORT_ID,
        "Get Knowledge cultivation run",
    ),
    (ENSURE_C4_EXPORT_ID, "Ensure C4 nucleus"),
    (DEBUG_C4_EXPORT_ID, "Debug C4 nucleus"),
    (
        ASSISTANT_LIST_DEPENDENTS_EXPORT_ID,
        "Assistant Knowledge dependents",
    ),
    (ASSISTANT_SEARCH_EXPORT_ID, "Assistant Knowledge search"),
    (ASSISTANT_LIST_EXPORT_ID, "Assistant Knowledge listing"),
    (ASSISTANT_GET_EXPORT_ID, "Assistant Knowledge read"),
    (
        ASSISTANT_FIND_ELEMENTS_EXPORT_ID,
        "Assistant semantic element search",
    ),
    (
        ASSISTANT_GET_ELEMENT_EXPORT_ID,
        "Assistant semantic element read",
    ),
];

#[derive(Clone, Copy)]
enum Invocation {
    PreviewTransfer,
    ApplyTransfer,
    Manifest,
    Projection,
    Create,
    Update,
    Delete,
    ListDependents,
    Get,
    List,
    ListAll,
    Search,
    FindElements,
    GetElement,
    Rebuild,
    Cultivate,
    GetRun,
    EnsureC4,
    DebugC4,
    Poll,
    HttpManifest,
    HttpSetup,
    HttpExport,
    HttpPage,
    HttpProjectSync,
    HttpC4,
    HttpC4Action,
    HttpC4Debug,
    HttpWrite,
    HttpCreateArtifact,
    HttpUpdateArtifact,
    HttpDeleteArtifact,
    HttpResolve,
    HttpEvents,
    HttpArtifacts,
    StorageTrigger,
}

struct InvocationDefinition {
    id: &'static str,
    invocation: Invocation,
}

const INVOCATIONS: &[InvocationDefinition] = &[
    invocation(PREVIEW_TRANSFER_EXPORT_ID, Invocation::PreviewTransfer),
    invocation(APPLY_TRANSFER_EXPORT_ID, Invocation::ApplyTransfer),
    invocation(MANIFEST_EXPORT_ID, Invocation::Manifest),
    invocation(PROJECTION_EXPORT_ID, Invocation::Projection),
    invocation(CREATE_EXPORT_ID, Invocation::Create),
    invocation(ASSISTANT_CREATE_EXPORT_ID, Invocation::Create),
    invocation(UPDATE_EXPORT_ID, Invocation::Update),
    invocation(ASSISTANT_UPDATE_EXPORT_ID, Invocation::Update),
    invocation(LIST_DEPENDENTS_EXPORT_ID, Invocation::ListDependents),
    invocation(
        ASSISTANT_LIST_DEPENDENTS_EXPORT_ID,
        Invocation::ListDependents,
    ),
    invocation(DELETE_EXPORT_ID, Invocation::Delete),
    invocation(GET_EXPORT_ID, Invocation::Get),
    invocation(ASSISTANT_GET_EXPORT_ID, Invocation::Get),
    invocation(LIST_EXPORT_ID, Invocation::List),
    invocation(ASSISTANT_LIST_EXPORT_ID, Invocation::List),
    invocation(LIST_ALL_EXPORT_ID, Invocation::ListAll),
    invocation(SEARCH_EXPORT_ID, Invocation::Search),
    invocation(ASSISTANT_SEARCH_EXPORT_ID, Invocation::Search),
    invocation(FIND_ELEMENTS_EXPORT_ID, Invocation::FindElements),
    invocation(ASSISTANT_FIND_ELEMENTS_EXPORT_ID, Invocation::FindElements),
    invocation(GET_ELEMENT_EXPORT_ID, Invocation::GetElement),
    invocation(ASSISTANT_GET_ELEMENT_EXPORT_ID, Invocation::GetElement),
    invocation(REBUILD_EXPORT_ID, Invocation::Rebuild),
    invocation(RUN_CULTIVATION_EXPORT_ID, Invocation::Cultivate),
    invocation(GET_CULTIVATION_RUN_EXPORT_ID, Invocation::GetRun),
    invocation(ENSURE_C4_EXPORT_ID, Invocation::EnsureC4),
    invocation(DEBUG_C4_EXPORT_ID, Invocation::DebugC4),
    invocation(ARTIFACT_GENERATION_POLL_EXPORT_ID, Invocation::Poll),
    invocation(HTTP_MANIFEST_EXPORT_ID, Invocation::HttpManifest),
    invocation(HTTP_SETUP_EXPORT_ID, Invocation::HttpSetup),
    invocation(HTTP_EXPORT_EXPORT_ID, Invocation::HttpExport),
    invocation(HTTP_PAGE_EXPORT_ID, Invocation::HttpPage),
    invocation(HTTP_SYNC_EXPORT_ID, Invocation::HttpProjectSync),
    invocation(HTTP_C4_EXPORT_ID, Invocation::HttpC4),
    invocation(HTTP_C4_ACTION_EXPORT_ID, Invocation::HttpC4Action),
    invocation(HTTP_C4_DEBUG_EXPORT_ID, Invocation::HttpC4Debug),
    invocation(HTTP_WRITE_EXPORT_ID, Invocation::HttpWrite),
    invocation(
        HTTP_CREATE_ARTIFACT_EXPORT_ID,
        Invocation::HttpCreateArtifact,
    ),
    invocation(
        HTTP_UPDATE_ARTIFACT_EXPORT_ID,
        Invocation::HttpUpdateArtifact,
    ),
    invocation(
        HTTP_DELETE_ARTIFACT_EXPORT_ID,
        Invocation::HttpDeleteArtifact,
    ),
    invocation(HTTP_RESOLVE_TARGET_EXPORT_ID, Invocation::HttpResolve),
    invocation(HTTP_EVENTS_EXPORT_ID, Invocation::HttpEvents),
    invocation(
        HTTP_PROJECTION_ARTIFACTS_EXPORT_ID,
        Invocation::HttpArtifacts,
    ),
    invocation(ELEMENT_TRIGGER_EXPORT_ID, Invocation::StorageTrigger),
];

const fn invocation(id: &'static str, invocation: Invocation) -> InvocationDefinition {
    InvocationDefinition { id, invocation }
}

fn invocation_for(capability_id: &str) -> Option<Invocation> {
    INVOCATIONS
        .iter()
        .find(|definition| definition.id == capability_id)
        .map(|definition| definition.invocation)
}

pub(crate) fn source(target: &str, executable_sha256: &str) -> PluginManifest {
    let executable = format!("bin/lumvise-plugin-knowledge-{target}");
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
        host_capabilities: host_capabilities(),
    }
}

fn exports() -> Vec<ExportDescriptor> {
    let mut exports = mcp_exports();
    exports.push(descriptor(
        crate::PROJECTION_EXPORT_ID,
        "Project Knowledge",
        ExportSurface::Command,
    ));
    exports.extend(http_exports());
    exports.extend(trigger_exports());
    exports.push(descriptor(
        crate::ARTIFACT_GENERATION_POLL_EXPORT_ID,
        "Consume generated functional artifacts",
        ExportSurface::RecurringTask {
            interval_seconds: 5,
            delivery: background_delivery_policy(),
        },
    ));
    exports
}

fn mcp_exports() -> Vec<ExportDescriptor> {
    ALL_MCP_EXPORTS
        .iter()
        .map(|(id, name)| {
            let surface = if id.starts_with("knowledge.") {
                ExportSurface::ScopedMcpTool {
                    scope: "assistant_session".into(),
                }
            } else if *id == crate::MANIFEST_EXPORT_ID {
                ExportSurface::Command
            } else {
                ExportSurface::McpTool
            };
            descriptor(id, name, surface)
        })
        .collect()
}

fn http_exports() -> Vec<ExportDescriptor> {
    use crate::*;
    [
        (
            HTTP_MANIFEST_EXPORT_ID,
            "Knowledge manifest",
            HttpMethod::Get,
            "/api/knowledge/manifest",
            HttpStreamMode::Buffered,
        ),
        (
            HTTP_SETUP_EXPORT_ID,
            "Knowledge setup",
            HttpMethod::Get,
            "/api/knowledge/setup",
            HttpStreamMode::Buffered,
        ),
        (
            HTTP_EXPORT_EXPORT_ID,
            "Knowledge projection export",
            HttpMethod::Get,
            "/api/knowledge/export",
            HttpStreamMode::Buffered,
        ),
        (
            HTTP_PAGE_EXPORT_ID,
            "Knowledge projection page",
            HttpMethod::Get,
            "/api/knowledge/page",
            HttpStreamMode::Buffered,
        ),
        (
            HTTP_SYNC_EXPORT_ID,
            "Knowledge Obsidian sync",
            HttpMethod::Post,
            "/api/knowledge/sync",
            HttpStreamMode::Buffered,
        ),
        (
            HTTP_C4_EXPORT_ID,
            "Knowledge C4 nucleus",
            HttpMethod::Post,
            "/api/knowledge/c4-nucleus",
            HttpStreamMode::Buffered,
        ),
        (
            HTTP_C4_ACTION_EXPORT_ID,
            "Knowledge C4 action",
            HttpMethod::Get,
            "/api/knowledge/c4-nucleus-action",
            HttpStreamMode::Buffered,
        ),
        (
            HTTP_C4_DEBUG_EXPORT_ID,
            "Knowledge C4 diagnostics",
            HttpMethod::Get,
            "/api/knowledge/c4-debug",
            HttpStreamMode::Buffered,
        ),
        (
            HTTP_WRITE_EXPORT_ID,
            "Knowledge write",
            HttpMethod::Post,
            "/api/knowledge/write",
            HttpStreamMode::Buffered,
        ),
        (
            HTTP_CREATE_ARTIFACT_EXPORT_ID,
            "Create Knowledge artifact",
            HttpMethod::Post,
            "/api/knowledge/artifacts",
            HttpStreamMode::Buffered,
        ),
        (
            HTTP_UPDATE_ARTIFACT_EXPORT_ID,
            "Update Knowledge artifact",
            HttpMethod::Post,
            "/api/knowledge/artifacts/update",
            HttpStreamMode::Buffered,
        ),
        (
            HTTP_DELETE_ARTIFACT_EXPORT_ID,
            "Delete Knowledge artifact",
            HttpMethod::Post,
            "/api/knowledge/artifacts/delete",
            HttpStreamMode::Buffered,
        ),
        (
            HTTP_RESOLVE_TARGET_EXPORT_ID,
            "Knowledge target resolution",
            HttpMethod::Post,
            "/api/knowledge/resolve-target",
            HttpStreamMode::Buffered,
        ),
        (
            HTTP_EVENTS_EXPORT_ID,
            "Knowledge live events",
            HttpMethod::Get,
            "/api/knowledge/events",
            HttpStreamMode::ServerSentEvents,
        ),
        (
            HTTP_PROJECTION_ARTIFACTS_EXPORT_ID,
            "Knowledge projection artifacts",
            HttpMethod::Get,
            "/api/knowledge/projection-artifacts",
            HttpStreamMode::Buffered,
        ),
    ]
    .into_iter()
    .map(|(id, name, method, path, stream_mode)| ExportDescriptor {
        description: guidance::description(id).into(),
        id: id.into(),
        name: name.into(),
        surface: ExportSurface::HttpRoute {
            method,
            path_template: path.into(),
            stream_mode,
            sse_policy: (stream_mode == HttpStreamMode::ServerSentEvents).then_some(
                SseStreamPolicy {
                    max_events_per_poll: 100,
                    poll_interval_ms: 100,
                    heartbeat_interval_ms: 15_000,
                    max_backoff_ms: 5_000,
                },
            ),
        },
        input_schema: if id == crate::HTTP_EVENTS_EXPORT_ID {
            http_stream_input_schema()
        } else {
            http_input_schema()
        },
        output_schema: http_output_schema(id),
        admission: None,
        execution: ExecutionMode::Foreground,
    })
    .collect()
}

fn trigger_exports() -> Vec<ExportDescriptor> {
    vec![ExportDescriptor {
        id: crate::ELEMENT_TRIGGER_EXPORT_ID.into(),
        name: "Semantic graph changes".into(),
        description: guidance::description(crate::ELEMENT_TRIGGER_EXPORT_ID).into(),
        surface: ExportSurface::StorageTrigger {
            event_kinds: vec![
                "semantic.element.upserted".into(),
                "semantic.element.deleted".into(),
            ],
            entity_kinds: Vec::new(),
            delivery: background_delivery_policy(),
        },
        input_schema: trigger_input_schema(),
        output_schema: trigger_output_schema(),
        admission: None,
        execution: ExecutionMode::Background,
    }]
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

fn descriptor(id: &str, name: &str, surface: ExportSurface) -> ExportDescriptor {
    ExportDescriptor {
        description: guidance::description(id).into(),
        id: id.into(),
        name: name.into(),
        surface,
        input_schema: input_schema(id),
        output_schema: output_schema(id),
        admission: None,
        execution: if matches!(
            id,
            crate::REBUILD_EXPORT_ID | crate::ARTIFACT_GENERATION_POLL_EXPORT_ID
        ) {
            ExecutionMode::Background
        } else {
            ExecutionMode::Foreground
        },
    }
}

fn host_capabilities() -> Vec<HostCapabilityRequirement> {
    [
        ("storage.plugin", "^1.0"),
        ("storage.semantic", "^1.4.0"),
        ("plugin.invoke", "^1.0"),
        ("neural.embed", "^1.0"),
        ("neural.llm", "^1.0"),
        ("runtime.project_execution", "^1.0"),
    ]
    .into_iter()
    .map(|(id, version)| HostCapabilityRequirement {
        id: id.into(),
        version: version.into(),
    })
    .collect()
}

fn input_schema(id: &str) -> Value {
    use crate::*;
    let schema = match id {
        PREVIEW_TRANSFER_EXPORT_ID => id_input("project_root"),
        APPLY_TRANSFER_EXPORT_ID => closed_object(
            &["project_root", "transfer_ids"],
            json!({"project_root": string(), "transfer_ids": array(string())}),
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
        UPDATE_EXPORT_ID | ASSISTANT_UPDATE_EXPORT_ID => {
            closed_object(&["artifact_id"], update_artifact_input_properties())
        }
        DELETE_EXPORT_ID => closed_object(
            &["artifact_id", "project_root"],
            json!({"artifact_id": string(), "project_root": string()}),
        ),
        GET_EXPORT_ID | ASSISTANT_GET_EXPORT_ID => id_input("artifact_id"),
        LIST_DEPENDENTS_EXPORT_ID | ASSISTANT_LIST_DEPENDENTS_EXPORT_ID => closed_object(
            &["target_kind", "target_id"],
            json!({"target_kind": string(), "target_id": string()}),
        ),
        LIST_EXPORT_ID | ASSISTANT_LIST_EXPORT_ID => id_input("semantic_element_id"),
        LIST_ALL_EXPORT_ID => closed_object(
            &[],
            json!({"semantic_element_ids": nullable(array(string())), "project_root": nullable(string()), "knowledge_type": nullable(knowledge_kind()),
                "tags": nullable(array(string())),
                "limit": nullable(json!({"type": "integer", "minimum": 0, "maximum": 1000}))}),
        ),
        SEARCH_EXPORT_ID | ASSISTANT_SEARCH_EXPORT_ID => closed_object(
            &["query"],
            json!({"query": string(), "project_root": nullable(string()),
                "semantic_element_id": nullable(string()), "knowledge_type": nullable(knowledge_kind()),
                "limit": nullable(json!({"type": "integer", "minimum": 0, "maximum": 1000}))}),
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

fn output_schema(id: &str) -> Value {
    use crate::*;
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
                "functionalArtifacts",
                "report",
            ],
            json!({"status": {"const": "ok"}, "projectRoot": string(),
                "requestedTarget": {"type": "object"}, "reportTarget": {"type": "object"},
                "scope": {"type": "object"}, "functionalArtifacts": array(json!({"type": "object"})),
                "report": {"type": "object"}}),
        ),
        ARTIFACT_GENERATION_POLL_EXPORT_ID => closed_object(
            &["pending", "completed"],
            json!({"pending": integer(), "completed": integer()}),
        ),
        _ => closed_object(&[], json!({})),
    }
}

fn create_artifact_input_properties() -> Value {
    json!({
        "artifact_id": string(), "semantic_element_id": string(),
        "knowledge_type": knowledge_kind(), "title": string(), "content": string(),
        "tags": array(string()), "dependencies": array(artifact_dependency()),
        "metadata": {"type": "object"}, "path": nullable(string()),
        "project_root": string()
    })
}

fn update_artifact_input_properties() -> Value {
    json!({
        "artifact_id": string(), "semantic_element_id": string(),
        "knowledge_type": nullable(knowledge_kind()), "title": nullable(string()),
        "content": nullable(string()), "tags": nullable(array(string())),
        "dependencies": nullable(array(artifact_dependency())),
        "metadata": nullable(json!({"type": "object"})), "path": nullable(string()),
        "project_root": nullable(string())
    })
}

fn artifact_schema() -> Value {
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
        ],
        json!({"artifact_id": string(), "semantic_element_id": string(), "knowledge_type": knowledge_kind(),
            "title": string(), "content": string(), "tags": array(string()), "metadata": {},
            "dependencies": array(artifact_dependency()),
            "path": nullable(string()),
            "project_root": nullable(string())}),
    )
}

fn element_schema() -> Value {
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
            "path": string(), "parent_element_id": nullable(string()), "start_line": nullable(integer()),
            "end_line": nullable(integer())}),
    )
}

fn search_output_schema() -> Value {
    closed_object(
        &["mode", "results"],
        json!({"mode": {"type": "string", "enum": ["vector", "lexical"]},
            "results": array(closed_object(
                &["artifact_id", "semantic_element_id", "knowledge_type", "title", "score"],
                json!({"artifact_id": string(), "semantic_element_id": string(),
                    "knowledge_type": knowledge_kind(), "title": string(), "score": number()}))) }),
    )
}

fn cultivation_run_schema() -> Value {
    closed_object(
        &[
            "run_id",
            "mode",
            "project_root",
            "artifact_ids",
            "abstraction_ids",
            "c4_artifact_ids",
            "nucleus_ids",
            "report_ids",
            "diagnostics_created",
            "updated_at",
        ],
        json!({"run_id": string(), "mode": string(), "project_root": string(),
            "artifact_ids": array(string()), "abstraction_ids": array(string()),
            "c4_artifact_ids": array(string()), "nucleus_ids": array(string()),
            "report_ids": array(string()), "diagnostics_created": boolean(), "updated_at": string()}),
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

fn http_input_schema() -> Value {
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

fn http_stream_input_schema() -> Value {
    let mut schema = http_input_schema();
    schema["properties"]["cursor"] = nullable(string());
    schema["properties"]["max_events"] = json!({"type": "integer", "minimum": 1, "maximum": 100});
    schema
}

fn http_output_schema(id: &str) -> Value {
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
    json!({"type": "object", "additionalProperties": false,
    "properties": {"status": string(), "created": boolean(), "artifact": artifact_schema(),
        "provider_id": nullable(string()), "model": nullable(string()), "request_id": string(),
        "target_fingerprint": string(), "missing_element_ids": array(string()),
        "pending_element_ids": array(string()), "submitted": integer(), "artifact_id": string(),
        "target_path": string(), "reason_code": string(), "reason": string(),
        "failed_element_ids": array(string()), "failure_reason": string()},
    "oneOf": [
        status_branch(&["status", "created", "artifact", "provider_id", "model"],
            &["failed_element_ids", "failure_reason"], "ready"),
        status_branch(&["status", "request_id", "target_fingerprint", "missing_element_ids",
            "pending_element_ids", "submitted", "artifact_id", "target_path"],
            &["failed_element_ids"], "pending"),
        required_status(&["status", "reason_code", "reason", "target_fingerprint",
            "artifact_id", "target_path"], "not_ready")
    ]})
}

fn http_c4_output_schema() -> Value {
    json!({"type": "object", "additionalProperties": false,
    "properties": {"status": string(), "created": boolean(), "artifactId": string(),
        "targetPath": string(), "requestId": string(), "targetFingerprint": string(),
        "missingElementIds": array(string()), "pendingElementIds": array(string()),
        "reasonCode": string(), "reason": string(), "failedElementIds": array(string())},
    "oneOf": [
        status_branch(&["status", "created", "artifactId", "targetPath"],
            &["failedElementIds"], "ready"),
        status_branch(&["status", "requestId", "targetFingerprint", "missingElementIds",
            "pendingElementIds", "artifactId", "targetPath"], &["failedElementIds"], "pending"),
        required_status(&["status", "reasonCode", "reason", "targetFingerprint",
            "artifactId", "targetPath"], "not_ready")
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

fn trigger_input_schema() -> Value {
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

fn trigger_output_schema() -> Value {
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

pub(crate) fn invoke(
    capability_id: &str,
    input: Value,
    context: &mut PluginContext<'_>,
    projection_cache: &crate::projection::ProjectProjectionCache,
) -> Result<Value, PluginError> {
    let invocation = invocation_for(capability_id)
        .ok_or_else(|| PluginError::unknown_capability(capability_id))?;
    invoke_export(invocation, capability_id, input, context, projection_cache)
}

fn invoke_export(
    invocation: Invocation,
    capability_id: &str,
    input: Value,
    context: &mut PluginContext<'_>,
    projection_cache: &crate::projection::ProjectProjectionCache,
) -> Result<Value, PluginError> {
    match invocation {
        Invocation::Manifest => Ok(metadata()),
        Invocation::Projection => crate::projection::command(input, context),
        Invocation::Poll => crate::artifact_generation::poll(context, None),
        Invocation::StorageTrigger => crate::events::storage_change(capability_id, input, context),
        other => invoke_artifact(other, capability_id, input, context, projection_cache),
    }
}

fn invoke_artifact(
    invocation: Invocation,
    capability_id: &str,
    input: Value,
    context: &mut PluginContext<'_>,
    projection_cache: &crate::projection::ProjectProjectionCache,
) -> Result<Value, PluginError> {
    match invocation {
        Invocation::PreviewTransfer => crate::transfer::preview(input, context),
        Invocation::ApplyTransfer => crate::transfer::apply(input, context),
        Invocation::Create => crate::create(input, context),
        Invocation::Update => crate::update(input, context),
        Invocation::Delete => crate::delete(input, context),
        Invocation::Get => crate::get(input, context),
        Invocation::List => crate::list(input, context),
        Invocation::ListAll => crate::list_all(input, context),
        Invocation::ListDependents => crate::list_dependents(input, context),
        Invocation::Search => crate::search(input, context),
        Invocation::FindElements => crate::semantic::search_elements(context, input),
        Invocation::GetElement => crate::get_element(input, context),
        Invocation::Rebuild => crate::rebuild_vectors(input, context),
        other => invoke_cultivation(other, capability_id, input, context, projection_cache),
    }
}

fn invoke_cultivation(
    invocation: Invocation,
    capability_id: &str,
    input: Value,
    context: &mut PluginContext<'_>,
    projection_cache: &crate::projection::ProjectProjectionCache,
) -> Result<Value, PluginError> {
    match invocation {
        Invocation::Cultivate => crate::cultivation::run(input, context),
        Invocation::GetRun => crate::cultivation::get_run(input, context),
        Invocation::EnsureC4 => crate::cultivation::ensure_c4(input, context),
        Invocation::DebugC4 => crate::cultivation::debug_c4(input, context),
        other => invoke_http(other, capability_id, input, context, projection_cache),
    }
}

fn invoke_http(
    invocation: Invocation,
    capability_id: &str,
    input: Value,
    context: &mut PluginContext<'_>,
    projection_cache: &crate::projection::ProjectProjectionCache,
) -> Result<Value, PluginError> {
    match invocation {
        Invocation::HttpManifest => crate::http::manifest(context),
        Invocation::HttpSetup => Ok(crate::http::setup(&input)),
        Invocation::HttpExport => crate::http::export(&input, context),
        Invocation::HttpPage => crate::http::page(&input, context),
        Invocation::HttpProjectSync => crate::http::sync(&input, context, projection_cache),
        Invocation::HttpC4 => crate::http::c4(&input, context),
        Invocation::HttpC4Action => Ok(crate::http::c4_action(&input)),
        Invocation::HttpC4Debug => crate::http::c4_debug(&input, context),
        Invocation::HttpWrite => crate::http::write(input, context),
        Invocation::HttpCreateArtifact => crate::http::create_artifact(input, context),
        Invocation::HttpUpdateArtifact => crate::http::update_artifact(input, context),
        Invocation::HttpDeleteArtifact => crate::http::delete_artifact(input, context),
        Invocation::HttpResolve => crate::http::resolve_target(&input, context),
        Invocation::HttpEvents => crate::http::events(&input, context),
        Invocation::HttpArtifacts => crate::http::projection_artifacts(context),
        _ => Err(PluginError::unknown_capability(capability_id)),
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
    fn create_knowledge_schema_requires_typed_known_fields_and_rejects_extras() {
        let schema = input_schema(CREATE_EXPORT_ID);

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["required"],
            serde_json::json!([
                "artifact_id",
                "semantic_element_id",
                "knowledge_type",
                "title",
                "content"
            ])
        );
        assert_eq!(schema["properties"]["artifact_id"]["type"], "string");
        assert_eq!(
            schema["properties"]["knowledge_type"]["enum"],
            serde_json::json!([
                "specification",
                "issue",
                "task_assignment",
                "definition",
                "annotation",
                "report",
                "decision",
                "manual_note",
                "derived_summary"
            ])
        );
    }
}
