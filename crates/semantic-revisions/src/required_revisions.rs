//! Exact dependency ownership shared by reads, archive validation and retention.
use crate::*;
use lumvise_contracts::ArtifactDependencyTarget;
use std::collections::{BTreeMap, BTreeSet};

impl SemanticRevisionBody {
    /// Lists exact parent/evidence dependencies; contextual attachment ownership belongs to the selection.
    /// Example: `resolved.body.intrinsic_dependencies()` when loading indexed facts.
    pub fn intrinsic_dependencies(&self) -> Vec<SemanticRecordRef> {
        match self {
            Self::Element(element) => element.required_revisions().collect(),
            Self::Artifact(artifact) => artifact.revision_dependencies(),
            Self::Pruned => vec![],
        }
    }
}

impl SemanticElement {
    /// Reads exact parent and payload requirements without following successors.
    /// Example: `element.required_revisions().collect::<Vec<_>>()`.
    pub fn required_revisions(&self) -> impl Iterator<Item = SemanticRecordRef> + '_ {
        self.parent_element_id
            .iter()
            .map(SemanticRecordRef::element)
            .chain(self.revision_dependencies.iter().cloned())
    }
}

impl SemanticArtifact {
    /// Lists exact evidence dependencies without copying an artifact body.
    /// Example: `artifact.revision_dependencies()` when collecting publication facts.
    pub fn revision_dependencies(&self) -> Vec<SemanticRecordRef> {
        self.dependencies
            .iter()
            .map(|dependency| match &dependency.target {
                ArtifactDependencyTarget::SemanticElement {
                    semantic_element_id,
                } => SemanticRecordRef::element(semantic_element_id),
                ArtifactDependencyTarget::Artifact { artifact_id, .. } => {
                    SemanticRecordRef::artifact(artifact_id)
                }
            })
            .collect()
    }
}

pub(crate) struct RevisionDependencyIndex<'a> {
    records: BTreeMap<SemanticRecordRef, &'a SemanticStoredRevision>,
    associations: BTreeMap<&'a str, Vec<&'a SemanticRevisionAssociation>>,
}

impl<'a> RevisionDependencyIndex<'a> {
    pub(crate) fn new(state: &'a SemanticRevisionState) -> Self {
        let mut associations = BTreeMap::<&str, Vec<_>>::new();
        for association in &state.associations {
            associations
                .entry(&association.artifact_id)
                .or_default()
                .push(association);
        }
        Self {
            records: crate::revision_index::record_index(state),
            associations,
        }
    }

    pub(crate) fn require(
        &self,
        reference: &SemanticRecordRef,
    ) -> SemanticRevisionResult<&'a SemanticStoredRevision> {
        crate::revision_index::require_body(&self.records, reference)?;
        Ok(self.records[reference])
    }

    pub(crate) fn requirements(
        &self,
        stored: &SemanticStoredRevision,
        expired: &BTreeSet<String>,
    ) -> Vec<SemanticRecordRef> {
        let mut references = stored.body.intrinsic_dependencies();
        let SemanticRevisionBody::Artifact(artifact) = &stored.body else {
            return references;
        };
        let associations = self
            .associations
            .get(artifact.artifact_id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        if associations.is_empty() {
            references.push(SemanticRecordRef::element(&artifact.semantic_element_id));
        }
        references.extend(
            associations
                .iter()
                .filter(|row| !expired.contains(&row.association_id))
                .map(|row| SemanticRecordRef::element(&row.semantic_element_id)),
        );
        references
    }
}
