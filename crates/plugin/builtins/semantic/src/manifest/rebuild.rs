//! Version-eight resumable rebuild schemas, separate from execution and persistence.
use super::schemas::{array, integer, nullable, object, string};
use serde_json::{Value, json};

pub(super) fn input_schema() -> Value {
    json!({"oneOf": [
        object(&["action", "project_root", "operation_id", "mode"], json!({
            "action": {"const": "start"}, "project_root": string(),
            "operation_id": string(), "mode": {"enum": ["full", "names", "text"]},
            "paths": array(string())
        })),
        continuation_request("continue"), continuation_request("cancel")
    ]})
}

fn continuation_request(action: &str) -> Value {
    object(
        &["action", "project_root", "run_id", "expected_revision"],
        json!({
            "action": {"const": action}, "project_root": string(),
            "run_id": string(), "expected_revision": {"type": "integer", "minimum": 0}
        }),
    )
}

pub(super) fn output_schema() -> Value {
    object(
        &[
            "project_root",
            "run_id",
            "revision",
            "status",
            "phase",
            "progress",
            "continuation",
            "index_state",
            "observed_source_revision",
            "failures",
        ],
        json!({
            "project_root": string(), "run_id": string(), "revision": integer(),
            "status": {"enum": ["pending", "complete", "conflicted", "canceled", "failed"]},
            "phase": {"enum": ["names", "text", "done"]}, "progress": progress_schema(),
            "continuation": nullable(object(&["run_id", "expected_revision"], json!({
                "run_id": string(), "expected_revision": integer()
            }))),
            "index_state": {"enum": ["missing", "stale", "rebuilding", "ready", "failed"]},
            "observed_source_revision": integer(), "failures": array(failure_schema())
        }),
    )
}

fn progress_schema() -> Value {
    object(
        &["names_completed", "sections_completed", "chunks_staged"],
        json!({
            "names_completed": integer(), "sections_completed": integer(),
            "chunks_staged": integer()
        }),
    )
}

fn failure_schema() -> Value {
    object(
        &["element_id", "path", "code", "message"],
        json!({
            "element_id": nullable(string()), "path": nullable(string()),
            "code": string(), "message": string()
        }),
    )
}
