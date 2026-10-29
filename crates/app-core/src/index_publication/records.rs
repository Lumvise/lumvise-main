use super::{Result, invalid_checkpoint};
use lumvise_contracts::{IndexBatchRequest, SemanticIndexObservation};
use lumvise_db_core::SemanticPartition;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexCheckoutBinding {
    pub client_instance_id: String,
    pub checkout_id: String,
    pub project_id: String,
    pub provider_instance_id: String,
    pub semantic_source_id: String,
    pub selection_id: String,
    pub source_context: String,
}

impl IndexCheckoutBinding {
    pub(super) fn validate(&self) -> Result<()> {
        for id in [
            &self.client_instance_id,
            &self.checkout_id,
            &self.project_id,
            &self.provider_instance_id,
            &self.semantic_source_id,
            &self.selection_id,
            &self.source_context,
        ] {
            if id.trim().is_empty() {
                return Err(invalid_checkpoint(
                    id,
                    "non-empty client, checkout, lineage and source identities",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexPathDigest {
    pub path: String,
    pub directory: bool,
    pub source_sha256: Option<String>,
    pub projection_sha256: Option<String>,
}

impl IndexPathDigest {
    pub(super) fn validate(&self) -> Result<()> {
        if self.path.contains('\\')
            || self
                .path
                .split('/')
                .any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(invalid_checkpoint(
                &self.path,
                "a canonical relative source path",
            ));
        }
        if self.directory != self.source_sha256.is_none() {
            return Err(invalid_checkpoint(
                &self.path,
                "a source checksum for files and no byte checksum for directories",
            ));
        }
        for digest in self
            .source_sha256
            .iter()
            .chain(self.projection_sha256.iter())
        {
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(invalid_checkpoint(digest, "a lowercase SHA-256 checksum"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenIndexPublication {
    pub operation_id: String,
    pub batch: IndexBatchRequest,
    pub inventory: Vec<IndexPathDigest>,
}

impl FrozenIndexPublication {
    pub(super) fn observation(&self) -> Result<&SemanticIndexObservation> {
        self.batch.revision_observation.as_ref().ok_or_else(|| {
            invalid_checkpoint(
                &self.operation_id,
                "the producer's frozen revision observation",
            )
        })
    }

    pub(super) fn partition(&self) -> SemanticPartition {
        let paths = self
            .batch
            .replace_paths
            .iter()
            .chain(&self.batch.removed_paths);
        SemanticPartition {
            project_root: self.batch.project_root.clone(),
            provider_instance_id: Some(self.batch.provider_instance_id.clone()),
            replace_paths: paths
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct CheckpointHeader {
    pub binding: IndexCheckoutBinding,
    pub generation: i64,
    pub producer_generation: i64,
    pub last_acknowledged_operation: Option<String>,
    pub pending: Option<PendingIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct PendingIdentity {
    pub operation_id: String,
    pub digest: String,
    pub ready: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct FrozenDescriptor {
    pub publication: FrozenIndexPublication,
    pub element_pages: usize,
    pub relationship_pages: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct CheckpointPath {
    pub digest: IndexPathDigest,
    pub elements: BTreeMap<String, String>,
    pub connections: BTreeMap<String, ObservedConnection>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ObservedConnection {
    pub owners: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ParserBinding {
    pub accepted_id: String,
    pub path: String,
}
