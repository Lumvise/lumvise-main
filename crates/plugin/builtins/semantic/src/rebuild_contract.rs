//! Rebuild wire contract. Source extraction, durable progress and execution stay internal.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RebuildSearchIndexRequest {
    Start {
        project_root: String,
        operation_id: String,
        mode: RebuildMode,
        paths: Option<Vec<String>>,
    },
    Continue {
        project_root: String,
        run_id: String,
        expected_revision: u64,
    },
    Cancel {
        project_root: String,
        run_id: String,
        expected_revision: u64,
    },
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RebuildMode {
    #[default]
    Full,
    Names,
    Text,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RebuildStatus {
    Pending,
    Complete,
    Conflicted,
    Canceled,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RebuildPhase {
    Names,
    Text,
    Done,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct RebuildProgress {
    pub names_completed: usize,
    pub sections_completed: usize,
    pub chunks_staged: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RebuildContinuation {
    pub run_id: String,
    pub expected_revision: u64,
}

/// Sanitized metadata only; never persist raw embedding error strings or source values.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RebuildFailure {
    pub element_id: Option<String>,
    pub path: Option<String>,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RebuildSearchIndexResponse {
    pub project_root: String,
    pub run_id: String,
    pub revision: u64,
    pub status: RebuildStatus,
    pub phase: RebuildPhase,
    pub progress: RebuildProgress,
    pub continuation: Option<RebuildContinuation>,
    pub index_state: String,
    pub observed_source_revision: i64,
    pub failures: Vec<RebuildFailure>,
}

#[cfg(test)]
mod tests {
    use super::RebuildSearchIndexRequest;
    use serde_json::json;

    #[test]
    fn continuation_requires_a_revision_and_rejects_source_payloads() {
        let missing_revision = json!({"action":"continue", "project_root":"/repo", "run_id":"run"});
        assert!(
            serde_json::from_value::<RebuildSearchIndexRequest>(missing_revision.clone()).is_err()
        );
        let mut valid = missing_revision;
        valid["expected_revision"] = json!(4);
        assert!(serde_json::from_value::<RebuildSearchIndexRequest>(valid.clone()).is_ok());
        valid["text"] = json!("caller-supplied source must not enter a rebuild");
        assert!(serde_json::from_value::<RebuildSearchIndexRequest>(valid).is_err());
    }

    #[test]
    fn legacy_page_retry_cannot_silently_start_a_new_run() {
        let legacy = json!({"project_root":"/repo", "mode":"text", "paths":["page.html"]});
        assert!(serde_json::from_value::<RebuildSearchIndexRequest>(legacy).is_err());
        let start = json!({"action":"start", "project_root":"/repo", "operation_id":"stable", "mode":"text"});
        assert!(
            matches!(serde_json::from_value::<RebuildSearchIndexRequest>(start).unwrap(),
            RebuildSearchIndexRequest::Start { operation_id, .. } if operation_id == "stable")
        );
    }
}
