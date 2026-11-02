use lumvise_plugin_package::{ExecutionMode, ExportDescriptor, ExportSurface};
use serde_json::{Value, json};

const CONTROLS: &[(&str, &str, &str)] = &[
    (
        "inspect_revision_integrity",
        "Inspect missing source revision bodies",
        "Inspect exact absent immutable bodies in the current durable source project without changing data. Requires direct owner provenance and source write access. Malformed or duplicate bodies fail inspection. Review project identity, commit and the complete missing list before explicitly approving recovery; no historical bytes can be reconstructed by acknowledgement.",
    ),
    (
        "recover_revision_integrity",
        "Acknowledge approved missing source bodies",
        "Apply only an explicitly approved inspection's exact project identity, expected commit, provider and complete missing-record list. Marks missing bodies unavailable and records an audit; preserves documents, selections and history. Does not recreate lost bytes. Direct native owner or bound owner MCP only; Agents and delegated calls are refused. Retry the identical operation ID and request after an uncertain acknowledgement; a changed observation requires a new inspection and approval.",
    ),
    (
        "read_revision_selection",
        "Read source selection",
        "Read exact selected records and original artifact edit tokens in an explicit project/selection. This read does not authorize a stale write.",
    ),
    (
        "select_revisions",
        "Select source revisions",
        "Conditionally select exact source revisions using the original generation and artifact edit tokens. Pinned selections freeze working artifacts and protect dependency closure. Retry the identical operation after a lost acknowledgement. Write access required; this is not a read/link citation-claim tool.",
    ),
    (
        "resolve_revision",
        "Read exact source revision",
        "Read one exact record in its project, returning its body or explicit pruned/unavailable state. Never substitute a selected successor for historical evidence.",
    ),
    (
        "preview_revision_retention",
        "Preview history cleanup",
        "Preview unneeded historical bodies using an explicit count or age policy. Current selections, pins and required dependencies remain protected. The returned plan is fenced against concurrent source changes; nothing is collected.",
    ),
    (
        "collect_revision_retention",
        "Collect previewed history",
        "Collect only the exact reviewed plan with its unchanged policy. Requires source write access. A newer publication or pin invalidates the plan; preview again rather than silently refreshing approval. Retry an uncertain acknowledgement with the identical plan.",
    ),
    (
        "get_revision_retention_preference",
        "Read cleanup preference",
        "Read this client's saved cleanup preference for the durable project identity. Default is thirty days. Automatic collection is disabled.",
    ),
    (
        "set_revision_retention_preference",
        "Save cleanup preference",
        "Save this client's age or revision-count preference with its original revision. This does not collect bodies, schedule cleanup or change source selections.",
    ),
];

pub(crate) fn supports(export: &str) -> bool {
    CONTROLS.iter().any(|(id, _, _)| *id == export)
}

pub(crate) fn exports() -> Vec<ExportDescriptor> {
    CONTROLS
        .iter()
        .map(|(id, name, description)| ExportDescriptor {
            id: (*id).into(),
            name: (*name).into(),
            description: (*description).into(),
            surface: ExportSurface::McpTool,
            input_schema: input(id),
            output_schema: json!({"type":"object"}),
            admission: None,
            execution: ExecutionMode::Foreground,
        })
        .collect()
}

fn input(id: &str) -> Value {
    match id {
        "read_revision_selection" => object(json!({"project_root":text(), "selection_id":text()})),
        "inspect_revision_integrity" => json!({
            "type":"object", "required":["project_root"], "additionalProperties":false,
            "properties":{"project_root":text(), "publication_operation_id":text()}
        }),
        "recover_revision_integrity" => object(json!({
            "operation_id":text(), "project_root":text(), "project_id":text(),
            "expected_commit_version":integer(0), "provider_instance_id":text(),
            "missing_records":{"type":"array", "minItems":1, "items":reference()}
        })),
        "select_revisions" => selection_update(),
        "resolve_revision" => object(json!({"project_root":text(), "record":reference()})),
        "preview_revision_retention" => {
            object(json!({"project_root":text(), "operation_id":text(), "policy":policy()}))
        }
        "collect_revision_retention" => object(json!({"plan":plan(), "policy":policy()})),
        "get_revision_retention_preference" => object(json!({"project_root":text()})),
        "set_revision_retention_preference" => object(
            json!({"project_root":text(), "expected_revision":integer(0), "policy":policy()}),
        ),
        _ => unreachable!("only registered source controls have schemas"),
    }
}

fn selection_update() -> Value {
    object(json!({
        "operation_id":text(), "project_root":text(), "project_id":text(),
        "selection_id":text(), "source_context":text(), "expected_generation":integer(0),
        "artifact_edits":{"type":"object", "additionalProperties":integer(1)},
        "mode":{"type":"string", "enum":["follow_accepted", "pinned"]},
        "records":{"type":"array", "items":reference()},
        "connection_ids":{"type":"array", "items":text()},
        "source_anchor":{"anyOf":[{"type":"null"}, object(json!({
            "revision":text(), "canonical_remote":{"type":["string", "null"]}, "dirty":{"type":"boolean"}
        }))]}
    }))
}

fn policy() -> Value {
    json!({"oneOf":[
        object(json!({"mode":{"const":"age"}, "history_age_seconds":integer(1)})),
        object(json!({"mode":{"const":"revision_count"}, "revisions":integer(1)}))
    ]})
}

fn plan() -> Value {
    object(json!({
        "operation_id":text(), "project_root":text(), "project_id":text(),
        "expected_commit_version":integer(0), "evaluated_at_ms":integer(0),
        "records":{"type":"array", "items":reference()},
        "expired_association_ids":{"type":"array", "items":text()}
    }))
}

fn reference() -> Value {
    object(json!({"kind":{"type":"string", "enum":["element", "artifact"]}, "record_id":text()}))
}

fn object(properties: Value) -> Value {
    let required: Vec<_> = properties
        .as_object()
        .expect("schema properties")
        .keys()
        .cloned()
        .collect();
    json!({"type":"object", "required":required, "properties":properties, "additionalProperties":false})
}

fn text() -> Value {
    json!({"type":"string", "minLength":1})
}
fn integer(minimum: u64) -> Value {
    json!({"type":"integer", "minimum":minimum})
}
