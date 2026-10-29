//! Portable record state shared by persistence rows and archive encoding.
use crate::*;
use serde::{Deserialize, Serialize};

/// Identity and lifecycle metadata remain after collection; payload bytes live in their owning body table.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionRecordHeader {
    pub record: SemanticRecordRef,
    pub element_scope: Option<SemanticRevisionElementScope>,
    pub canonical_lifecycle: Option<String>,
    pub artifact_state: Option<SemanticArtifactRevisionState>,
    pub created_at_ms: i64,
    pub unselected_at_ms: Option<i64>,
    pub pruned_at_ms: Option<i64>,
}

impl From<&SemanticStoredRevision> for SemanticRevisionRecordHeader {
    fn from(stored: &SemanticStoredRevision) -> Self {
        Self {
            record: stored.record.clone(),
            element_scope: stored.element_scope.clone(),
            canonical_lifecycle: match &stored.body {
                SemanticRevisionBody::Element(element) => Some(element.lifecycle.clone()),
                _ => None,
            },
            artifact_state: stored.artifact_state.clone(),
            created_at_ms: stored.created_at_ms,
            unselected_at_ms: stored.unselected_at_ms,
            pruned_at_ms: stored.pruned_at_ms,
        }
    }
}

impl From<SemanticStoredRevision> for SemanticRevisionRecordHeader {
    fn from(stored: SemanticStoredRevision) -> Self {
        Self::from(&stored)
    }
}

impl SemanticRevisionRecordHeader {
    /// Restores a canonical body without changing exact identity or resurrecting collected content.
    /// Example: `header.restore(SemanticRevisionBody::Pruned)?` for an explicit tombstone.
    pub fn restore(
        self,
        mut body: SemanticRevisionBody,
    ) -> SemanticRevisionResult<SemanticStoredRevision> {
        let identity = match &mut body {
            SemanticRevisionBody::Element(element) => {
                element.lifecycle = self.canonical_lifecycle.clone().ok_or_else(|| {
                    SemanticRevisionError::invalid_value(
                        &self.record.record_id,
                        "the canonical element lifecycle",
                    )
                })?;
                Some(SemanticRecordRef::element(&element.semantic_element_id))
            }
            SemanticRevisionBody::Artifact(artifact) => {
                Some(SemanticRecordRef::artifact(&artifact.artifact_id))
            }
            SemanticRevisionBody::Pruned => None,
        };
        if identity
            .as_ref()
            .is_some_and(|identity| identity != &self.record)
            || identity.is_none() != self.pruned_at_ms.is_some()
            || (self.record.kind == SemanticRecordKind::Artifact) != self.artifact_state.is_some()
            || self
                .artifact_state
                .as_ref()
                .is_some_and(|state| state.edit_version == 0)
        {
            return Err(SemanticRevisionError::invalid_value(
                &self.record.record_id,
                "the exact canonical body matching its available/pruned revision header",
            ));
        }
        Ok(SemanticStoredRevision {
            record: self.record,
            element_scope: self.element_scope,
            body,
            artifact_state: self.artifact_state,
            created_at_ms: self.created_at_ms,
            unselected_at_ms: self.unselected_at_ms,
            pruned_at_ms: self.pruned_at_ms,
        })
    }
}
