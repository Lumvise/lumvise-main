use super::*;

pub(super) const PAGE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TransferHeader {
    pub format_release: u32,
    pub project_id: String,
    pub source_root: String,
    pub archive_commit: String,
    pub selection_id: String,
    pub snapshot_id: String,
    pub display_name: Option<String>,
    pub source_revision: Option<String>,
    pub canonical_remote: Option<String>,
}

impl TransferHeader {
    pub fn parse(input: &str) -> SemanticRevisionResult<Self> {
        let header: Self = serde_json::from_str(input)?;
        crate::revision_index::require_project_identity(&header.project_id)?;
        if header.format_release != 2
            || header.selection_id != "working"
            || integer(&header.archive_commit)? < 0
        {
            return Err(invalid(
                input,
                "release 2 working archive with a nonnegative commit",
            ));
        }
        crate::validation::require_non_empty(&header.source_root, "an archive source root")?;
        Ok(header)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TransferPage {
    pub format_release: u32,
    pub header: Option<TransferHeader>,
    pub rows: Vec<StorageRow>,
}

impl TransferPage {
    pub(super) fn validate(&self, header: &TransferHeader) -> SemanticRevisionResult<()> {
        if self.format_release != 2 || self.header.as_ref().is_some_and(|value| value != header) {
            return Err(invalid(
                self.format_release.to_string(),
                "a release 2 page belonging to its frozen header",
            ));
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::policy_bytes) struct ImportContext {
    pub project_root: String,
    pub project_id: String,
    pub authenticated_actor: String,
    pub operation_id: String,
    pub archive_digest: String,
    pub expected_commit: String,
    pub archive_commit: String,
    pub now_ms: String,
}

impl ImportContext {
    pub(super) fn validate(&self, header: &TransferHeader) -> SemanticRevisionResult<()> {
        for value in [
            &self.project_root,
            &self.authenticated_actor,
            &self.operation_id,
        ] {
            crate::validation::require_non_empty(
                value,
                "a nonempty archive route, actor and operation identity",
            )?;
        }
        if self.project_id != header.project_id
            || self.archive_commit != header.archive_commit
            || integer(&self.expected_commit)? < 0
        {
            return Err(invalid(
                &self.project_id,
                "the frozen archive lineage and commit",
            ));
        }
        if self.archive_digest.len() != 64
            || !self.archive_digest.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(invalid(
                &self.archive_digest,
                "the exact frozen archive SHA-256",
            ));
        }
        integer(&self.now_ms)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Catalog {
    Incoming,
    Current,
    Candidate,
    Scratch,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Lookup {
    Key {
        key: StorageRowKey,
    },
    Scan {
        row_kind: Option<String>,
        after: Option<StorageRowKey>,
        target_bytes: usize,
    },
    Index {
        name: String,
        value: String,
        after: Option<StorageRowKey>,
        target_bytes: usize,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Query {
    pub query_id: String,
    pub catalog: Catalog,
    pub lookup: Lookup,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::policy_bytes) struct ImportFact {
    pub query_id: String,
    pub rows: Vec<StorageRow>,
    pub next: Option<StorageRowKey>,
}

#[derive(Serialize)]
pub(super) struct Write {
    pub catalog: Catalog,
    pub rows: Vec<StorageRow>,
    pub removed: Vec<StorageRowKey>,
}

#[derive(Default, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    #[default]
    Replay,
    RetainIncoming,
    ValidateIncoming,
    CopyCurrent,
    MergeIncoming,
    ReconcileRecords,
    RetainCandidate,
    ValidateCandidate,
    CountRemoved,
    Finish,
}

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Counts {
    pub added_elements: usize,
    pub added_artifacts: usize,
    pub kept_artifacts: usize,
    pub reused: usize,
    pub inactive: usize,
    pub connections: usize,
}

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Checkpoint {
    #[serde(default)]
    pub binding: String,
    pub phase: Phase,
    pub after: Option<StorageRowKey>,
    pub owner: Option<StorageRowKey>,
    pub offset: usize,
    pub section: usize,
    #[serde(default)]
    pub generation: i64,
    #[serde(default)]
    pub cursor: Option<StorageRowKey>,
    #[serde(default)]
    pub dirty: bool,
    pub counts: Counts,
}

#[derive(Serialize)]
pub(super) struct Acceptance {
    pub expected_commit: String,
    pub commit_version: String,
    pub outcome_json: String,
    pub receipt_row: StorageRow,
}

#[derive(Serialize)]
pub(super) struct Step {
    pub state: &'static str,
    pub checkpoint_json: String,
    pub queries: Vec<Query>,
    pub writes: Vec<Write>,
    pub acceptance: Option<Acceptance>,
}
