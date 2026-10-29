//! Applies document provenance before disclosing titles, vector hits or exact bodies.
use super::*;
use lumvise_contracts::KnowledgeDocumentAccess;
use lumvise_db_core::{
    SemanticRecordRef, SemanticRelationship, SemanticRevisionBody, VectorSearchResult,
};
use serde_json::Value;

impl KnowledgeSemanticAccess<'_> {
    pub(super) fn visible_relationships(
        &self,
        edges: Vec<SemanticRelationship>,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<Vec<SemanticRelationship>> {
        let mut visible = Vec::new();
        for edge in edges {
            if !self.root_allowed(&edge.project_root, scope, false) {
                continue;
            }
            if is_memory_root(&edge.project_root)
                && (!self.visible_element_id(&edge.source_element_id, scope)?
                    || !self.visible_element_id(&edge.target_element_id, scope)?)
            {
                continue;
            }
            visible.push(edge);
        }
        Ok(visible)
    }

    fn visible_element_id(
        &self,
        id: &str,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<bool> {
        self.element(id)?
            .as_ref()
            .map(|element| self.element_allowed(element, scope))
            .transpose()
            .map(|visible| visible.unwrap_or(false))
    }

    pub(super) fn element_allowed(
        &self,
        element: &SemanticElement,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<bool> {
        if !self.root_allowed(&element.project_root, scope, false)
            || !self.metadata_allowed(&element.metadata, scope)?
        {
            return Ok(false);
        }
        self.require_root(&element.project_root, scope, false)?;
        Ok(true)
    }

    pub(super) fn metadata_allowed(
        &self,
        metadata: &Value,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<bool> {
        let metadata = metadata.get("indexer_metadata").unwrap_or(metadata);
        let access = metadata["knowledge_document"]
            .get("access")
            .or_else(|| metadata["knowledge_document_deleted"]["document"].get("access"));
        let (Some(value), Some(scope)) = (access, scope) else {
            return Ok(true);
        };
        let access: KnowledgeDocumentAccess = serde_json::from_value(value.clone())
            .map_err(|_| denied("invalid stored document provenance"))?;
        if scope
            .space
            .as_ref()
            .is_none_or(|space| space.space_id != access.space_id)
            || access.required_projects.iter().any(|project| {
                !scope
                    .project_grants
                    .iter()
                    .any(|grant| grant.project.same_project(project))
            })
        {
            return Ok(false);
        }
        self.authority
            .observe_access(self.context, value, scope, self.control)
            .map_err(|error| DbError::invalid_value("document provenance", error.to_string()))?;
        Ok(true)
    }

    pub(super) fn filter_vectors(
        &self,
        hits: Vec<VectorSearchResult>,
        artifacts: bool,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<SemanticResult> {
        let mut allowed = Vec::new();
        for hit in hits {
            let visible = if artifacts {
                match self.semantic.execute(
                    SemanticOperation::Artifact {
                        artifact_id: hit.id.clone(),
                    },
                    self.control,
                )? {
                    SemanticResult::Artifact(Some(artifact)) => {
                        self.artifact_allowed(&artifact, scope)?
                    }
                    _ => false,
                }
            } else {
                self.element(&hit.id)?
                    .as_ref()
                    .map(|element| self.element_allowed(element, scope))
                    .transpose()?
                    .unwrap_or(false)
            };
            if visible {
                allowed.push(hit);
            }
        }
        Ok(if artifacts {
            SemanticResult::ArtifactVectorSearch(allowed)
        } else {
            SemanticResult::ElementVectorSearch(allowed)
        })
    }

    pub(super) fn filter_revision(
        &self,
        result: SemanticResult,
        root: &str,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<SemanticResult> {
        let SemanticResult::RevisionResolution(Some(revision)) = result else {
            return Ok(result);
        };
        let allowed = match &revision.body {
            SemanticRevisionBody::Element(element) => self.element_allowed(element, scope)?,
            SemanticRevisionBody::Artifact(artifact) => {
                self.revision_artifact_allowed(artifact, root, scope)?
            }
            SemanticRevisionBody::Pruned => true,
        };
        Ok(SemanticResult::RevisionResolution(
            allowed.then_some(revision),
        ))
    }

    fn revision_artifact_allowed(
        &self,
        artifact: &SemanticArtifact,
        root: &str,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<bool> {
        if !self.metadata_allowed(&artifact.metadata, scope)? {
            return Ok(false);
        }
        if !is_memory_root(root) {
            return Ok(true);
        }
        let result = self.semantic.execute(
            SemanticOperation::ResolveRevision {
                project_root: root.into(),
                record: SemanticRecordRef::element(&artifact.semantic_element_id),
            },
            self.control,
        )?;
        match result {
            SemanticResult::RevisionResolution(Some(revision)) => match revision.body {
                SemanticRevisionBody::Element(owner) => self.element_allowed(&owner, scope),
                _ => Ok(false),
            },
            _ => Ok(false),
        }
    }
}
