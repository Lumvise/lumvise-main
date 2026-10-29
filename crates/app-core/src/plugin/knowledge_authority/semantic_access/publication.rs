//! Annotations inherit the document's access; they cannot become unrestricted memory.
use super::*;
use lumvise_contracts::KnowledgeDocumentAccess;
use lumvise_db_core::SemanticRevisionPublication;
use serde_json::Value;

impl KnowledgeSemanticAccess<'_> {
    pub(super) fn require_memory_publication(
        &self,
        publication: &SemanticRevisionPublication,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<()> {
        if !is_memory_root(&publication.project_root) {
            return Ok(());
        }
        for write in &publication.artifacts {
            let owner = publication.structure.as_ref().and_then(|structure| {
                structure.elements.iter().find(|element| {
                    element.semantic_element_id == write.artifact.semantic_element_id
                })
            });
            self.require_memory_artifact(&write.artifact, owner, scope)?;
        }
        Ok(())
    }

    pub(super) fn require_memory_artifact(
        &self,
        artifact: &SemanticArtifact,
        incoming: Option<&SemanticElement>,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<()> {
        let stored = if incoming.is_none() {
            self.element(&artifact.semantic_element_id)?
        } else {
            None
        };
        let Some(owner) = incoming.or(stored.as_ref()) else {
            return Ok(());
        };
        if !is_memory_root(&owner.project_root) {
            return Ok(());
        }
        let scope = scope.ok_or_else(|| denied(&owner.project_root))?;
        let metadata = owner
            .metadata
            .get("indexer_metadata")
            .unwrap_or(&owner.metadata);
        let access = artifact.metadata["knowledge_document"]
            .get("access")
            .or_else(|| metadata["knowledge_document"].get("access"));
        self.require_memory_write_context(access, scope, &artifact.artifact_id)
    }

    fn require_memory_write_context(
        &self,
        value: Option<&Value>,
        scope: &KnowledgeAccessContext,
        artifact: &str,
    ) -> lumvise_db_core::Result<()> {
        let access: Option<KnowledgeDocumentAccess> = value
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| denied(artifact))?;
        let covered = scope.source_projects.iter().all(|project| {
            access.as_ref().is_some_and(|access| {
                access
                    .required_projects
                    .iter()
                    .any(|required| required.same_project(project))
            })
        });
        if !covered
            || access.as_ref().is_some_and(|access| {
                scope
                    .space
                    .as_ref()
                    .is_none_or(|space| space.space_id != access.space_id)
            })
        {
            return Err(DbError::invalid_value(
                artifact,
                "update the owning Knowledge document to retain supplied project context before annotating it",
            ));
        }
        Ok(())
    }
}
