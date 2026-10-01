use super::indexing::SemanticIndexLogRecord;
use crate::{McpCapability, McpInstanceStatus};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectManifestRecord {
    pub project_root: String,
    pub display_name: String,
    pub provider_instance_ids: Vec<String>,
    pub capabilities: Vec<McpCapability>,
    pub status: McpInstanceStatus,
    pub latest_seen_at: Option<String>,
    pub latest_semantic_index_log: Option<SemanticIndexLogRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectManifestResponse {
    pub projects: Vec<ProjectManifestRecord>,
}
