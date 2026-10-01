use crate::local::grafeo::graph_rows::{
    apply_artifact_deletion, apply_element_deactivation, prepare_artifact_deletion,
    prepare_element_deactivation,
};
use crate::local::grafeo::semantic_storage::SemanticStorage;
use crate::local::sql::artifact_blobs::ArtifactBlobRepository;
use crate::local::sql::validation::require_non_empty;
use crate::{ChangeDisposition, Result, SemanticArtifact};

impl<'db> SemanticStorage<'db> {
    /// Removes one semantic artifact and every SQL blob it owns (its content
    /// payload and attachments such as canvas images).
    pub fn remove_artifact(&self, artifact_id: &str) -> Result<bool> {
        require_non_empty(artifact_id, "non-empty semantic artifact id")?;
        let (removed, artifact) = self.commit_preplanned_graph_write(
            |database| Ok(prepare_artifact_deletion(database, artifact_id)),
            |graph, commit_version, collector, plan| {
                let Some(artifact) = plan.artifact.clone() else {
                    return Ok((false, None));
                };
                if let Some(owner) = apply_artifact_deletion(graph, commit_version, plan) {
                    collector.record_element(&owner, ChangeDisposition::Upserted);
                }
                Ok((true, Some(artifact)))
            },
        )?;
        if removed && let Some(artifact) = artifact.as_ref() {
            self.remove_blob_for_artifact(artifact)?;
        }
        Ok(removed)
    }

    /// Deactivates one semantic element while retaining its tombstone, then removes
    /// owned artifacts and graph relationships.
    pub fn remove_element(&self, semantic_element_id: &str) -> Result<bool> {
        require_non_empty(semantic_element_id, "non-empty semantic element id")?;
        let (removed, artifacts) = self.commit_preplanned_graph_write(
            |database| Ok(prepare_element_deactivation(database, semantic_element_id)),
            |graph, commit_version, collector, plan| {
                if !plan.exists {
                    return Ok((false, Vec::new()));
                }
                let artifacts = plan.artifacts.clone();
                collector.record_removal(
                    semantic_element_id,
                    &plan.project_root,
                    &plan.entity_kind,
                );
                Ok((
                    apply_element_deactivation(graph, commit_version, plan),
                    artifacts,
                ))
            },
        )?;
        if removed {
            self.remove_blobs_for_artifacts(&artifacts)?;
        }
        Ok(removed)
    }

    fn remove_blob_for_artifact(&self, artifact: &SemanticArtifact) -> Result<()> {
        let blobs = ArtifactBlobRepository::with_clock(self.conn, self.clock.clone());
        if let Some(content_ref) = artifact.content_ref.as_deref() {
            blobs.delete_blob(content_ref)?;
        }
        blobs.delete_blobs_for_artifact(&artifact.artifact_id)?;
        Ok(())
    }

    fn remove_blobs_for_artifacts(&self, artifacts: &[SemanticArtifact]) -> Result<()> {
        for artifact in artifacts {
            self.remove_blob_for_artifact(artifact)?;
        }
        Ok(())
    }
}
