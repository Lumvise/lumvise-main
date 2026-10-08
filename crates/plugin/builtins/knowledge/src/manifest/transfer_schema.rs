use super::schema::{array, boolean, closed_object, integer, string};
use serde_json::{Value, json};

pub(super) fn preview() -> Value {
    closed_object(
        &["project_root", "existing_artifact_count", "candidates"],
        json!({
            "project_root": string(), "existing_artifact_count": integer(),
            "candidates": array(candidate())
        }),
    )
}

fn candidate() -> Value {
    let properties = json!({
        "transfer_id": string(), "source_artifact_id": string(),
        "source_project_root": string(), "source_semantic_element_id": string(),
        "title": string(), "knowledge_type": string(),
        "target_semantic_element_id": string(), "target_name": string(), "target_path": string(),
        "exact_match": boolean(), "simhash_distance": integer(), "already_copied": boolean()
    });
    let mut required = properties
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    required.sort_unstable();
    closed_object(&required, properties.clone())
}

pub(super) fn applied() -> Value {
    closed_object(
        &[
            "project_root",
            "copied_artifact_ids",
            "already_copied_artifact_ids",
            "skipped_count",
        ],
        json!({
            "project_root": string(), "copied_artifact_ids": array(string()),
            "already_copied_artifact_ids": array(string()), "skipped_count": integer()
        }),
    )
}
