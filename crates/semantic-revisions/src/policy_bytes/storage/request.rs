//! Injects trusted storage facts without round-tripping domain counters through JavaScript.
use super::*;
use serde_json::{Map, Value, json};

pub(super) fn prepare(input: &str, actor: Option<String>) -> SemanticRevisionResult<StorageResult> {
    let (mut request, operation) = unbound(input)?;
    if operation == "blob_put" {
        let blob: content::staged::BlobPut = serde_json::from_str(input)?;
        return encoded(
            "intent",
            &read::ReadIntent::BlobWrite {
                content_ref: blob.content_ref,
                artifact_id: blob.artifact_id,
            },
        );
    }
    if operation == "archive_receipt" {
        return archives::receipt_intent(input, actor.as_deref());
    }
    if matches!(operation.as_str(), "import_archive" | "export_archive") {
        return archives::prepare(input);
    }
    if operation == "search_elements" {
        return names::prepare(input);
    }
    if matches!(
        operation.as_str(),
        "store_element_name_vectors" | "store_artifact_text_vectors"
    ) {
        let write: vectors::VectorWrite = serde_json::from_str(input)?;
        return encoded(
            "intent",
            &read::ReadIntent::VectorWrite {
                records: write.records(),
            },
        );
    }
    let fields: &[&str] = match operation.as_str() {
        "publication" | "selection" => &["operation", "request"],
        "citation_acquisition" | "citation_release" => {
            request.insert("authenticated_actor".into(), json!(require_actor(actor)?));
            &["operation", "request", "authenticated_actor"]
        }
        "membership" | "families" | "connections" => &["operation", "query"],
        "selected" => &["operation", "selection_id", "query"],
        "vectors" => &[
            "operation",
            "selection_id",
            "artifact",
            "engine_id",
            "model",
        ],
        "blob" => {
            let blob: content::BlobRequest =
                serde_json::from_value(Value::Object(request.clone()))?;
            if blob.selection_id.is_none() {
                return encoded(
                    "intent",
                    &read::ReadIntent::Content {
                        content_ref: blob.content_ref,
                    },
                );
            }
            request.insert("operation".into(), json!("selected"));
            request.insert(
                "query".into(),
                json!({"kind":"content","content_ref":blob.content_ref}),
            );
            &["operation", "selection_id", "query"]
        }
        "snapshot" | "selection_at" | "source_anchor" => &["operation", "selection_id"],
        "resolve" => &["operation", "record"],
        "history" | "retention_plan" | "collect" => {
            request.insert("operation".into(), json!("all"));
            &["operation"]
        }
        _ => {
            return Err(SemanticRevisionError::invalid_value(
                operation,
                "a stored revision operation",
            ));
        }
    };
    request.retain(|key, _| fields.contains(&key.as_str()));
    let intent: read::ReadIntent = serde_json::from_value(Value::Object(request))?;
    encoded("intent", &intent)
}

pub(super) fn execute(
    input: &str,
    state_json: &str,
    rows: &[StorageRow],
    actor: Option<String>,
    timestamp: &SemanticRevisionTimestamp,
    next: i64,
) -> SemanticRevisionResult<StorageResult> {
    let (mut request, operation) = unbound(input)?;
    let state: SemanticRevisionState = serde_json::from_str(state_json)?;
    if operation == "blob_put" {
        return content::staged::put(&state, input, rows, actor.as_deref(), timestamp, next);
    }
    if operation == "publication" {
        content::staged::claim(&state, input, actor.as_deref(), rows)?;
    }
    if operation == "archive_receipt" {
        return archives::receipt_read(input, actor.as_deref(), rows);
    }
    if matches!(operation.as_str(), "import_archive" | "export_archive") {
        return archives::execute(&state, input, rows, actor.as_deref(), timestamp, next);
    }
    if matches!(
        operation.as_str(),
        "store_element_name_vectors" | "store_artifact_text_vectors"
    ) {
        return vectors::write(&state, input, rows, next);
    }
    if operation == "vectors" {
        return vectors::read(&state, input, rows);
    }
    if operation == "blob" {
        return content::read_blob(&state, input, rows, &timestamp.rfc3339, actor.as_deref());
    }
    if operation == "search_elements" {
        names::prepare(input)?;
        return encoded(
            "snapshot",
            &state.snapshot(
                &field::<String>(&request, "selection_id")?,
                &timestamp.rfc3339,
            )?,
        );
    }
    if operation == "selected" {
        let read::ReadIntent::Selected { selection_id, .. } =
            serde_json::from_value(Value::Object(request))?
        else {
            unreachable!()
        };
        return encoded(
            "snapshot",
            &state.snapshot(&selection_id, &timestamp.rfc3339)?,
        );
    }
    if operation == "collect" {
        let plan = field::<SemanticRevisionRetentionPlan>(&request, "plan")?;
        let policy = field::<SemanticRevisionRetentionPolicy>(&request, "policy")?;
        if request.len() != 3 {
            return Err(SemanticRevisionError::invalid_value(
                input,
                "collect with only plan and policy",
            ));
        }
        return super::collection(
            state_json,
            &serde_json::to_string(&plan)?,
            &serde_json::to_string(&policy)?,
            timestamp.unix_ms,
            next,
        );
    }
    inject_facts(&mut request, &operation, state)?;
    inject_clock(&mut request, &operation, timestamp, next);
    if matches!(
        operation.as_str(),
        "citation_acquisition" | "citation_release"
    ) {
        request.insert("authenticated_actor".into(), json!(require_actor(actor)?));
    }
    super::opaque_evaluation(&serde_json::to_string(&request)?)
}

fn unbound(input: &str) -> SemanticRevisionResult<(Map<String, Value>, String)> {
    let request: Map<String, Value> = serde_json::from_str(input)?;
    for field in [
        "state",
        "basis",
        "authenticated_actor",
        "timestamp",
        "next_commit",
        "now_ms",
        "published_at",
    ] {
        if request.contains_key(field) {
            return Err(SemanticRevisionError::invalid_value(
                field,
                "gateway-owned facts absent from the caller request",
            ));
        }
    }
    let operation = field::<String>(&request, "operation")?;
    Ok((request, operation))
}

fn inject_facts(
    request: &mut Map<String, Value>,
    operation: &str,
    state: SemanticRevisionState,
) -> SemanticRevisionResult<()> {
    if operation == "publication" {
        let publication: SemanticRevisionPublication = field(request, "request")?;
        let basis = SemanticRevisionPublicationBasis {
            structure_coverage: structure_coverage(&publication),
            artifact_ids: requested_artifact_ids(&publication),
            facts: state,
        };
        request.insert("basis".into(), serde_json::to_value(basis)?);
    } else if operation == "families" {
        let query: SemanticRevisionFamilyQuery = field(request, "query")?;
        let head = state.selection(&query.selection_id).cloned();
        let basis = SemanticRevisionFamilyBasis {
            project_root: state.project_root,
            commit_version: state.commit_version,
            selection: head.as_ref().map(|head| head.selection.clone()),
            selected_records: head.map_or_else(Vec::new, |head| head.records),
            links: state.revision_links,
        };
        request.insert("basis".into(), serde_json::to_value(basis)?);
    } else {
        request.insert("state".into(), serde_json::to_value(state)?);
    }
    Ok(())
}

fn inject_clock(
    request: &mut Map<String, Value>,
    operation: &str,
    timestamp: &SemanticRevisionTimestamp,
    next: i64,
) {
    match operation {
        "publication" | "selection" | "citation_acquisition" | "citation_release" => {
            request.insert("timestamp".into(), json!(timestamp));
            request.insert("next_commit".into(), json!(next));
        }
        "snapshot" => {
            request.insert("published_at".into(), json!(timestamp.rfc3339));
        }
        "retention_plan" => {
            request.insert("now_ms".into(), json!(timestamp.unix_ms));
        }
        _ => {}
    }
}

fn field<T: serde::de::DeserializeOwned>(
    request: &Map<String, Value>,
    name: &str,
) -> SemanticRevisionResult<T> {
    Ok(serde_json::from_value(
        request.get(name).cloned().ok_or_else(|| {
            SemanticRevisionError::invalid_value(name, "a required stored request field")
        })?,
    )?)
}

fn require_actor(actor: Option<String>) -> SemanticRevisionResult<String> {
    actor.filter(|actor| !actor.is_empty()).ok_or_else(|| {
        SemanticRevisionError::invalid_value(
            "absent actor",
            "an authenticated gateway principal for citation retention",
        )
    })
}
