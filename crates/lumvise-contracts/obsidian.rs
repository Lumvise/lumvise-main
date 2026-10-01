//! Shared wire contract between the Knowledge projection and Obsidian consumers.
//!
//! The module owns serialization shape and semantic validation helpers for contract
//! values that cross process boundaries.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

pub const OBSIDIAN_SYNC_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObsidianSyncRequestV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub source_id: String,
    pub applied_revision: Option<i64>,
    #[serde(default)]
    pub content_hashes: BTreeMap<String, String>,
}

impl ObsidianSyncRequestV1 {
    /// Validates a request payload from raw JSON text and returns a typed request.
    ///
    /// ```
    /// use lumvise_contracts::ObsidianSyncRequestV1;
    /// let request = ObsidianSyncRequestV1::from_json(
    ///     r#"{"schemaVersion":1,"sourceId":"/repo","contentHashes":{}}"#,
    /// ).unwrap();
    /// assert_eq!(request.source_id, "/repo");
    /// ```
    pub fn from_json(input: &str) -> Result<Self, ObsidianSyncContractError> {
        Self::validate(serde_json::from_str(input)?)
    }

    /// Validates a request payload from a decoded JSON value.
    ///
    /// ```
    /// use serde_json::json;
    /// use lumvise_contracts::ObsidianSyncRequestV1;
    /// let value = json!({"schemaVersion":1,"sourceId":"/repo","contentHashes":{}});
    /// let request = ObsidianSyncRequestV1::from_value(value).unwrap();
    /// assert_eq!(request.source_id, "/repo");
    /// ```
    pub fn from_value(input: Value) -> Result<Self, ObsidianSyncContractError> {
        Self::validate(serde_json::from_value(input)?)
    }

    fn validate(request: Self) -> Result<Self, ObsidianSyncContractError> {
        if !request.source_id.trim().is_empty() {
            return Ok(request);
        }
        Err(invalid_contract(
            "sourceId `` is empty; expected a non-empty project root".into(),
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObsidianSyncBatchV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub source_id: String,
    pub base_revision: Option<i64>,
    pub target_revision: i64,
    pub full_reset: bool,
    pub generated_at: String,
    pub sync_token: String,
    pub spaces: Vec<KnowledgeSpaceV1>,
    pub page_hashes: BTreeMap<String, String>,
    pub changed_pages: Vec<ProjectedPageV1>,
    pub deleted_page_ids: Vec<String>,
}

impl ObsidianSyncBatchV1 {
    /// Validates a full sync batch from JSON text.
    ///
    /// ```
    /// use lumvise_contracts::ObsidianSyncBatchV1;
    /// let payload = r#"{"schemaVersion":1,"sourceId":"/repo","targetRevision":1,"fullReset":true,"generatedAt":"2026-01-01T00:00:00Z","syncToken":"abc","spaces":[],"pageHashes":{},"changedPages":[],"deletedPageIds":[]}"#;
    /// let batch = ObsidianSyncBatchV1::from_json(payload).unwrap();
    /// assert_eq!(batch.target_revision, 1);
    /// ```
    pub fn from_json(input: &str) -> Result<Self, ObsidianSyncContractError> {
        let batch: Self = serde_json::from_str(input)?;
        batch.validate()?;
        Ok(batch)
    }

    fn validate(&self) -> Result<(), ObsidianSyncContractError> {
        validate_revision_range(self.base_revision, self.target_revision)?;
        validate_changed_pages(self)?;
        validate_deleted_pages(self)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KnowledgeSpaceV1 {
    pub space_id: String,
    pub title: String,
    pub status: String,
    pub write_policy: KnowledgeWritePolicyV1,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectedPageV1 {
    pub element_id: String,
    pub space: String,
    pub kind: String,
    pub title: String,
    pub markdown: String,
    pub content_md5: String,
    pub path_hint: String,
    pub source_id: String,
    pub source_refs: Vec<KnowledgeSourceRefV1>,
    pub sync_token: String,
    pub change_marker: String,
    pub ownership: KnowledgeOwnershipV1,
    pub write_policy: KnowledgeWritePolicyV1,
    pub children: Vec<KnowledgeElementChildV1>,
    pub artifacts: Vec<KnowledgeArtifactV1>,
    pub link_targets: Vec<KnowledgeLinkTargetV1>,
    pub properties: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KnowledgeSourceRefV1 {
    pub entity_kind: String,
    pub entity_id: String,
    pub path: String,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KnowledgeElementChildV1 {
    pub element_id: String,
    pub kind: String,
    pub title: String,
    pub path_hint: Option<String>,
    pub source_refs: Vec<KnowledgeSourceRefV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KnowledgeArtifactV1 {
    pub artifact_id: String,
    pub title: String,
    pub markdown: String,
    pub owner_element_id: String,
    pub owner_title: String,
    pub owner_kind: String,
    pub content_md5: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KnowledgeLinkTargetV1 {
    pub element_id: String,
    pub title: String,
    pub note_path_hint: String,
    pub heading: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum KnowledgeWritePolicyV1 {
    Annotation,
    Readonly,
    Generated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeOwnershipV1 {
    pub content: KnowledgeOwnerV1,
    pub source: KnowledgeOwnerV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum KnowledgeOwnerV1 {
    Lumvise,
    Obsidian,
}

#[derive(Debug)]
pub enum ObsidianSyncContractError {
    Decode(serde_json::Error),
    Invalid(String),
}

impl Display for ObsidianSyncContractError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(error) => write!(
                formatter,
                "invalid Obsidian sync JSON `{error}`; expected ObsidianSyncBatchV1"
            ),
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ObsidianSyncContractError {}

impl From<serde_json::Error> for ObsidianSyncContractError {
    fn from(error: serde_json::Error) -> Self {
        Self::Decode(error)
    }
}

fn deserialize_schema_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version == OBSIDIAN_SYNC_SCHEMA_VERSION {
        return Ok(version);
    }
    Err(serde::de::Error::custom(format!(
        "unsupported schemaVersion `{version}`; expected `{OBSIDIAN_SYNC_SCHEMA_VERSION}`"
    )))
}

fn validate_revision_range(
    base_revision: Option<i64>,
    target_revision: i64,
) -> Result<(), ObsidianSyncContractError> {
    if base_revision.is_none_or(|base| base <= target_revision) {
        return Ok(());
    }
    Err(invalid_contract(format!(
        "targetRevision `{target_revision}` precedes baseRevision `{base_revision:?}`; expected monotonic revisions"
    )))
}

fn validate_changed_pages(batch: &ObsidianSyncBatchV1) -> Result<(), ObsidianSyncContractError> {
    let mut page_ids = BTreeSet::new();
    for page in &batch.changed_pages {
        validate_changed_page(batch, page, &mut page_ids)?;
    }
    Ok(())
}

fn validate_changed_page(
    batch: &ObsidianSyncBatchV1,
    page: &ProjectedPageV1,
    page_ids: &mut BTreeSet<String>,
) -> Result<(), ObsidianSyncContractError> {
    if page.source_id != batch.source_id {
        return Err(invalid_contract(format!(
            "changed page sourceId `{}` differs from batch sourceId `{}`; expected one source",
            page.source_id, batch.source_id
        )));
    }
    validate_unique_page_id(page, page_ids)?;
    validate_page_hash(batch, page)
}

fn validate_unique_page_id(
    page: &ProjectedPageV1,
    page_ids: &mut BTreeSet<String>,
) -> Result<(), ObsidianSyncContractError> {
    if page_ids.insert(page.element_id.clone()) {
        return Ok(());
    }
    Err(invalid_contract(format!(
        "duplicate changed page `{}`; expected unique elementId values",
        page.element_id
    )))
}

fn validate_page_hash(
    batch: &ObsidianSyncBatchV1,
    page: &ProjectedPageV1,
) -> Result<(), ObsidianSyncContractError> {
    if batch.page_hashes.get(&page.element_id) == Some(&page.content_md5) {
        return Ok(());
    }
    Err(invalid_contract(format!(
        "page hash for `{}` is `{:?}`; expected `{}`",
        page.element_id,
        batch.page_hashes.get(&page.element_id),
        page.content_md5
    )))
}

fn validate_deleted_pages(batch: &ObsidianSyncBatchV1) -> Result<(), ObsidianSyncContractError> {
    let changed = batch
        .changed_pages
        .iter()
        .map(|page| page.element_id.as_str())
        .collect::<BTreeSet<_>>();
    let conflicting = batch
        .deleted_page_ids
        .iter()
        .find(|page_id| changed.contains(page_id.as_str()));
    match conflicting {
        None => Ok(()),
        Some(page_id) => Err(invalid_contract(format!(
            "page `{page_id}` is both changed and deleted; expected disjoint operations"
        ))),
    }
}

fn invalid_contract(message: String) -> ObsidianSyncContractError {
    ObsidianSyncContractError::Invalid(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_FIXTURE: &str = include_str!(
        "../plugin/builtins/knowledge/tests/fixtures/contracts/obsidian_sync_v1/full_batch_page.json"
    );

    #[test]
    fn full_batch_fixture_round_trips() {
        let batch = ObsidianSyncBatchV1::from_json(FULL_FIXTURE).unwrap();
        let encoded = serde_json::to_string(&batch).unwrap();
        let decoded = ObsidianSyncBatchV1::from_json(&encoded).unwrap();
        assert_eq!(decoded, batch);
    }

    #[test]
    fn sync_request_decodes_versioned_revision_and_hashes() {
        let request = ObsidianSyncRequestV1::from_json(
            r#"{"schemaVersion":1,"sourceId":"/repo","appliedRevision":40,"contentHashes":{"file:a":"md5:a"}}"#,
        )
        .unwrap();
        assert_eq!(request.applied_revision, Some(40));
    }

    #[test]
    fn unknown_schema_version_is_rejected() {
        let invalid = FULL_FIXTURE.replace("\"schemaVersion\": 1", "\"schemaVersion\": 2");
        let error = ObsidianSyncBatchV1::from_json(&invalid).unwrap_err();
        assert!(error.to_string().contains("expected `1`"));
    }

    #[test]
    fn target_revision_before_base_is_rejected() {
        let invalid = FULL_FIXTURE.replace("\"targetRevision\": 42", "\"targetRevision\": 39");
        let error = ObsidianSyncBatchV1::from_json(&invalid).unwrap_err();
        assert!(error.to_string().contains("expected monotonic revisions"));
    }

    #[test]
    fn changed_page_without_matching_hash_is_rejected() {
        let invalid = FULL_FIXTURE.replace(
            "\"file:src/billing.rs\": \"md5:billing\"",
            "\"file:src/billing.rs\": \"md5:unexpected\"",
        );
        let error = ObsidianSyncBatchV1::from_json(&invalid).unwrap_err();
        assert!(error.to_string().contains("expected `md5:billing`"));
    }
}
