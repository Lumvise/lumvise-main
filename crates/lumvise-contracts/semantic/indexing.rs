use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticSourceUpsert {
    pub semantic_source_id: String,
    pub kind: String,
    pub name: String,
    pub root_path: Option<String>,
    pub root_uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticElementUpsert {
    pub semantic_source_id: String,
    pub semantic_element_id: String,
    pub path: String,
    pub semantic_element_type: String,
    pub semantic_element_name: String,
    pub parent_element_id: Option<String>,
    pub content_fingerprint: Option<String>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticRelationshipUpsert {
    pub source_element_id: String,
    pub target_element_id: Option<String>,
    pub relationship_kind: String,
    pub relationship_label: String,
    pub target_label: Option<String>,
    pub target_locator: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexBatchRequest {
    pub provider_instance_id: String,
    pub project_root: String,
    #[serde(default)]
    pub replace_paths: Vec<String>,
    #[serde(default)]
    pub removed_paths: Vec<String>,
    pub semantic_sources: Vec<SemanticSourceUpsert>,
    pub semantic_elements: Vec<SemanticElementUpsert>,
    pub semantic_relationships: Vec<SemanticRelationshipUpsert>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticIndexSnapshotRef {
    pub snapshot_id: String,
    pub completed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticIndexSnapshotFreshnessStatus {
    Unknown,
    Unavailable,
    Unindexed,
    Fresh,
    Stale,
    Running,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticIndexSnapshotFreshness {
    pub status: SemanticIndexSnapshotFreshnessStatus,
    #[serde(default)]
    pub latest_completed_snapshot: Option<SemanticIndexSnapshotRef>,
    #[serde(default)]
    pub active_ingestion_job_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticIndexJobStatus {
    Accepted,
    Queued,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexBatchResponse {
    pub accepted: bool,
    pub semantic_sources_upserted: usize,
    pub semantic_elements_upserted: usize,
    pub semantic_relationships_upserted: usize,
    #[serde(default)]
    pub ingestion_job_id: Option<String>,
    #[serde(default)]
    pub ingestion_status: Option<SemanticIndexJobStatus>,
    #[serde(default)]
    pub snapshot_freshness: Option<SemanticIndexSnapshotFreshness>,
}

impl IndexBatchResponse {
    pub fn accepted_async(batch: &IndexBatchRequest, ingestion_job_id: impl Into<String>) -> Self {
        let ingestion_job_id = ingestion_job_id.into();
        Self::accepted_async_with_freshness(
            batch,
            ingestion_job_id.clone(),
            SemanticIndexSnapshotFreshness {
                status: SemanticIndexSnapshotFreshnessStatus::Unknown,
                latest_completed_snapshot: None,
                active_ingestion_job_id: Some(ingestion_job_id),
            },
        )
    }

    pub fn accepted_async_with_freshness(
        batch: &IndexBatchRequest,
        ingestion_job_id: impl Into<String>,
        mut snapshot_freshness: SemanticIndexSnapshotFreshness,
    ) -> Self {
        let ingestion_job_id = ingestion_job_id.into();
        snapshot_freshness.active_ingestion_job_id = Some(ingestion_job_id.clone());
        Self {
            accepted: true,
            semantic_sources_upserted: batch.semantic_sources.len(),
            semantic_elements_upserted: batch.semantic_elements.len(),
            semantic_relationships_upserted: batch.semantic_relationships.len(),
            ingestion_job_id: Some(ingestion_job_id.clone()),
            ingestion_status: Some(SemanticIndexJobStatus::Accepted),
            snapshot_freshness: Some(snapshot_freshness),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticIndexLogRequest {
    pub provider_instance_id: String,
    #[serde(default)]
    pub semantic_source_id: Option<String>,
    #[serde(default)]
    pub plugin_id: Option<String>,
    pub index_log_id: String,
    pub status: String,
    #[serde(default)]
    pub metrics: Option<Value>,
    pub content_fingerprint: Option<String>,
    pub error: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticIndexLogRecord {
    pub provider_instance_id: String,
    pub semantic_source_id: Option<String>,
    pub plugin_id: Option<String>,
    pub index_log_id: String,
    pub status: String,
    pub metrics: Option<Value>,
    pub content_fingerprint: Option<String>,
    pub error: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticIndexLogResponse {
    pub index_log: Option<SemanticIndexLogRecord>,
}
