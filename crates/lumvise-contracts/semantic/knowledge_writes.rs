//! Passive Knowledge acknowledgements for producer recovery; these records never pin content.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The original public Knowledge operation, not the current state of its target.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeWriteAction {
    /// Initial creation or an explicit recreation.
    Create,
    /// Conditional replacement of the caller's observed revision.
    Update,
    /// Conditional removal of the caller's observed revision.
    Delete,
}

/// Durable producer progress, independent of current membership and body availability.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeWriteState {
    /// No acknowledgement exists for the original action and retry identity.
    NotFound,
    /// The owner has frozen input but has not recorded a terminal result.
    Pending,
    /// The original operation was accepted.
    Accepted,
    /// The original operation's observed editing base conflicted.
    Conflicted,
}

/// Reads a known artifact write without loading or keeping its historical body.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeArtifactWriteReceiptRequest {
    /// A route resolving to the operation's original durable project.
    pub project_root: String,
    /// The explicit identity supplied to the original write.
    pub operation_id: String,
    /// The original public artifact operation.
    pub action: KnowledgeWriteAction,
}

/// The exact artifact accepted or removed by the operation; this does not pin its body.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeArtifactRevisionReference {
    /// The original owning project route.
    pub project_root: String,
    /// An exact artifact revision, never a successor chosen at receipt-read time.
    pub artifact_id: String,
}

/// Body-independent artifact acknowledgement for cooperating producer journals.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeArtifactWriteReceipt {
    /// The caller's original retry identity.
    pub operation_id: String,
    /// Knowledge's durable publication identity.
    pub publication_id: String,
    /// The route used for this scoped acknowledgement read.
    pub project_root: String,
    /// Progress of the original operation.
    pub state: KnowledgeWriteState,
    /// The accepted exact identity, available after payload collection.
    pub artifact_revision: Option<KnowledgeArtifactRevisionReference>,
    /// Counter of the accepted body; deletion has no replacement body.
    pub lifecycle_revision: Option<u64>,
    /// Original semantic acceptance order.
    pub commit_version: Option<i64>,
}
