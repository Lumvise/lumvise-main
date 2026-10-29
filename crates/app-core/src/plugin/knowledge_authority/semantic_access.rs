//! The controlled storage boundary prevents raw Semantic calls from bypassing
//! Knowledge. Private bodies flow only through its signed document exports.
mod disclosure;
mod publication;
use super::{KnowledgeAuthority, agent, is_memory_root};
use lumvise_contracts::{KnowledgeAccessContext, KnowledgePrincipalKind};
use lumvise_db_core::{
    DbError, ProjectSnapshotScope, ScopedSemanticRead, SemanticArtifact, SemanticElement,
    SemanticOperation, SemanticPersistence, SemanticResult,
};
use lumvise_plugin_runtime::PluginInvocationContext;
use lumvise_resource_routing::InvocationControl;

pub(in crate::plugin) struct KnowledgeSemanticAccess<'a> {
    pub(in crate::plugin) semantic: &'a dyn SemanticPersistence,
    pub(in crate::plugin) authority: &'a KnowledgeAuthority,
    pub(in crate::plugin) context: &'a PluginInvocationContext,
    pub(in crate::plugin) control: &'a InvocationControl,
}

impl KnowledgeSemanticAccess<'_> {
    pub(in crate::plugin) fn execute(
        &self,
        operation: SemanticOperation,
    ) -> lumvise_db_core::Result<SemanticResult> {
        let scope = self.current()?;
        self.authorize(&operation, scope.as_ref())?;
        let revision_root = match &operation {
            SemanticOperation::ResolveRevision { project_root, .. } => Some(project_root.clone()),
            _ => None,
        };
        let writes_blob = matches!(&operation, SemanticOperation::ArtifactBlobPut { .. });
        if let SemanticOperation::SearchElementCandidates {
            project_root: None,
            query,
            limit,
        } = operation
        {
            let result = self.search_sources(&query, limit, scope.as_ref())?;
            self.current()?;
            return Ok(result);
        }
        let result = self.semantic.execute(operation, self.control)?;
        // Session retirement or project removal during I/O must invalidate delivery.
        let current = self.current()?;
        // A copied attachment is staged before its new artifact is published.
        if writes_blob {
            return Ok(result);
        }
        let result = if let Some(root) = revision_root {
            self.filter_revision(result, &root, current.as_ref())?
        } else {
            self.filter(result, current.as_ref())?
        };
        self.current()?;
        Ok(result)
    }

    fn current(&self) -> lumvise_db_core::Result<Option<KnowledgeAccessContext>> {
        if agent::agent_frame(self.context).is_none()
            && super::inspection::inspection_frame(self.context).is_none()
            && !matches!(
                self.context.scope_id(),
                Some("assistant_session" | lumvise_contracts::KNOWLEDGE_MCP_SCOPE | "agent_run")
            )
        {
            return Ok(None);
        }
        self.authority
            .current(self.semantic, self.context, self.control)
            .map(Some)
            .map_err(|error| DbError::invalid_value("memory scope", error.to_string()))
    }

    fn root_allowed(
        &self,
        root: &str,
        scope: Option<&KnowledgeAccessContext>,
        write: bool,
    ) -> bool {
        if is_memory_root(root) {
            let owned_agent =
                scope.is_some_and(|scope| scope.principal_kind == KnowledgePrincipalKind::Agent);
            return (self.document_call() || owned_agent)
                && (!write || scope.is_some_and(super::project_access::memory_writable))
                && scope
                    .and_then(|scope| scope.space.as_ref())
                    .is_some_and(|space| space.project_root == root);
        }
        scope.is_none_or(|scope| {
            scope
                .project_grants
                .iter()
                .any(|grant| grant.project.project_root == root && (!write || grant.writable))
        })
    }

    fn require_root(
        &self,
        root: &str,
        scope: Option<&KnowledgeAccessContext>,
        write: bool,
    ) -> lumvise_db_core::Result<()> {
        if !self.root_allowed(root, scope, write) {
            return Err(denied(root));
        }
        if !is_memory_root(root) {
            self.authority
                .observe_project(self.context, root, scope, self.control)
                .map_err(|error| DbError::invalid_value("memory provenance", error.to_string()))?;
        }
        Ok(())
    }

    fn document_call(&self) -> bool {
        self.context.plugin_chain().iter().any(|frame| {
            frame.plugin_id == "builtin.knowledge"
                && matches!(
                    frame.export_id.as_str(),
                    "resolve_document_space"
                        | "inspect_document_space"
                        | "execute_document_delegation"
                        | "configure_document_hook"
                        | "get_document_hook"
                        | "document.space"
                        | "resolve_document_reference"
                        | "document.reference"
                        | "create_document"
                        | "document.create"
                        | "get_document"
                        | "document.get"
                        | "update_document"
                        | "document.update"
                        | "delete_document"
                        | "document.delete"
                        | "list_documents"
                        | "document.list"
                        | "search_documents"
                        | "document.search"
                        | "get_document_write_receipt"
                        | "get_document_citation_retention"
                        | "reconcile_document_citations"
                )
        })
    }

    fn authorize(
        &self,
        operation: &SemanticOperation,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<()> {
        use SemanticOperation::*;
        match operation {
            ProjectIdentity { project_root }
            | ProjectAccess { project_root }
            | ElementsByIds { project_root, .. }
            | ElementsByIdsIncludingInactive { project_root, .. }
            | ProjectArtifacts { project_root, .. }
            | ResolveRevision { project_root, .. }
            | RevisionSelection { project_root, .. }
            | RevisionSelectionAt { project_root, .. }
            | RevisionSourceAnchor { project_root, .. } => {
                self.require_root(project_root, scope, false)
            }
            PublishRevisions(publication) => {
                self.require_root(&publication.project_root, scope, true)?;
                self.require_memory_publication(publication, scope)
            }
            RevisionFamilies(query) => self.require_root(&query.project_root, scope, false),
            RevisionSelectionMembers(query) => self.require_root(&query.project_root, scope, false),
            ScopedRead(request) => self.authorize_scoped(request, scope),
            UpsertArtifact { artifact, .. } => {
                self.require_element(&artifact.semantic_element_id, scope, true)?;
                self.require_memory_artifact(artifact, None, scope)
            }
            RemoveElement {
                semantic_element_id,
            } => self.require_element(semantic_element_id, scope, true),
            RemoveArtifact { artifact_id } => self.require_artifact(artifact_id, scope, true),
            _ => self.authorize_other(operation, scope),
        }
    }

    fn authorize_other(
        &self,
        operation: &SemanticOperation,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<()> {
        use SemanticOperation::*;
        match operation {
            SearchElementCandidates {
                project_root: Some(root),
                ..
            } => self.require_root(root, scope, false),
            SearchElementNameVectors { project_root, .. }
            | SearchArtifactTextVectors { project_root, .. } => {
                self.require_root(project_root, scope, false)
            }
            StoreElementNameVectors { project_root, .. }
            | StoreArtifactTextVectors { project_root, .. } => {
                self.require_root(project_root, scope, true)
            }
            ProjectSnapshot {
                scope: selected, ..
            } => self.authorize_snapshot(selected, scope),
            SelectiveSubgraph { project_root, .. }
            | ProjectElementCounts { project_root }
            | ProjectArchive { project_root }
            | RevisionHistory { project_root }
            | CreatePzSnapshot { project_root, .. }
            | ImportPzSnapshot { project_root, .. }
            | BeginProjectSnapshot { project_root, .. } => self.require_source(project_root, scope),
            SyncStructure { project_root, .. } => {
                self.require_source(project_root, scope)?;
                self.require_root(project_root, scope, true)
            }
            SyncPartition { partition, .. } => {
                self.require_source(&partition.project_root, scope)?;
                self.require_root(&partition.project_root, scope, true)
            }
            ProjectRendererGraph(query) => self.require_source(&query.project_root, scope),
            ArtifactBlobPut { artifact_id, .. } => self.require_artifact(artifact_id, scope, true),
            SelectRevisions(update) => {
                if update.selection_id.starts_with("knowledge-citation:") {
                    return Err(denied(&update.selection_id));
                }
                self.require_source_write(&update.project_root, scope)
            }
            PlanRevisionRetention { project_root, .. } => {
                self.require_source_write(project_root, scope)
            }
            CollectRevisionRetention { plan, .. } => {
                self.require_source_write(&plan.project_root, scope)
            }
            // Identity-only queries and unscoped rows are filtered before delivery.
            ProjectRoots
            | SemanticRevision
            | Element { .. }
            | Artifact { .. }
            | ArtifactsByIds { .. }
            | RelationshipsFrom { .. }
            | RelationshipsTouchingElements { .. }
            | ArtifactsForElements { .. }
            | ArtifactsForElementWithInheritance { .. }
            | ArtifactDependents { .. }
            | ArtifactBlobGet { .. }
            | CandidateSourceElements { .. }
            | SearchElementCandidates {
                project_root: None, ..
            } => Ok(()),
            other => Err(denied(&format!("unsupported scoped operation {other:?}"))),
        }
    }

    fn authorize_scoped(
        &self,
        request: &ScopedSemanticRead,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<()> {
        match request {
            ScopedSemanticRead::Location { project_root, .. }
            | ScopedSemanticRead::PathPrefix { project_root, .. } => {
                self.require_root(project_root, scope, false)
            }
            ScopedSemanticRead::Structure {
                scope: ProjectSnapshotScope::ProjectRoot(root),
                ..
            } => self.require_root(root, scope, false),
            ScopedSemanticRead::Structure {
                scope: ProjectSnapshotScope::SemanticElement(id),
                ..
            }
            | ScopedSemanticRead::Dependency {
                semantic_element_id: id,
                ..
            } => self.require_element(id, scope, false),
        }
    }

    fn authorize_snapshot(
        &self,
        selected: &ProjectSnapshotScope,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<()> {
        match selected {
            ProjectSnapshotScope::ProjectRoot(root) => self.require_source(root, scope),
            ProjectSnapshotScope::SemanticElement(id) => {
                if let Some(element) = self.element(id)? {
                    return self.require_source(&element.project_root, scope);
                }
                Ok(())
            }
        }
    }

    fn require_source(
        &self,
        root: &str,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<()> {
        if is_memory_root(root) {
            return Err(denied(root));
        }
        self.require_root(root, scope, false)
    }

    fn require_source_write(
        &self,
        root: &str,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<()> {
        self.require_source(root, scope)?;
        self.require_root(root, scope, true)
    }

    fn require_element(
        &self,
        id: &str,
        scope: Option<&KnowledgeAccessContext>,
        write: bool,
    ) -> lumvise_db_core::Result<()> {
        if let Some(element) = self.element(id)? {
            self.require_root(&element.project_root, scope, write)?;
            if !self.element_allowed(&element, scope)? {
                return Err(denied(id));
            }
        }
        Ok(())
    }

    fn require_artifact(
        &self,
        id: &str,
        scope: Option<&KnowledgeAccessContext>,
        write: bool,
    ) -> lumvise_db_core::Result<()> {
        let result = self.semantic.execute(
            SemanticOperation::Artifact {
                artifact_id: id.into(),
            },
            self.control,
        )?;
        if let SemanticResult::Artifact(Some(artifact)) = result {
            self.require_element(&artifact.semantic_element_id, scope, write)?;
        }
        Ok(())
    }

    fn element(&self, id: &str) -> lumvise_db_core::Result<Option<SemanticElement>> {
        match self.semantic.execute(
            SemanticOperation::Element {
                semantic_element_id: id.into(),
            },
            self.control,
        )? {
            SemanticResult::Element(element) => Ok(element),
            _ => Err(denied(id)),
        }
    }

    fn artifact_allowed(
        &self,
        artifact: &SemanticArtifact,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<bool> {
        let Some(owner) = self.element(&artifact.semantic_element_id)? else {
            return Ok(false);
        };
        if !self.element_allowed(&owner, scope)? {
            return Ok(false);
        }
        self.require_root(&owner.project_root, scope, false)?;
        self.metadata_allowed(&artifact.metadata, scope)
    }

    fn search_sources(
        &self,
        query: &str,
        limit: usize,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<SemanticResult> {
        let SemanticResult::ProjectRoots(roots) = self
            .semantic
            .execute(SemanticOperation::ProjectRoots, self.control)?
        else {
            return Err(denied("project roots"));
        };
        let mut elements = Vec::new();
        for root in roots
            .iter()
            .filter(|root| !is_memory_root(root) && self.root_allowed(root, scope, false))
        {
            self.require_root(root, scope, false)?;
            let result = self.semantic.execute(
                SemanticOperation::SearchElementCandidates {
                    project_root: Some(root.clone()),
                    query: query.into(),
                    limit,
                },
                self.control,
            )?;
            if let SemanticResult::Elements(found) = result {
                elements.extend(found);
            }
        }
        elements.truncate(limit);
        Ok(SemanticResult::Elements(elements))
    }

    fn filter(
        &self,
        result: SemanticResult,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<SemanticResult> {
        match result {
            SemanticResult::ProjectRoots(roots) => Ok(SemanticResult::ProjectRoots(
                roots
                    .into_iter()
                    .filter(|root| !is_memory_root(root) && self.root_allowed(root, scope, false))
                    .collect(),
            )),
            SemanticResult::Element(element) => {
                let element = match element {
                    Some(element) if self.element_allowed(&element, scope)? => Some(element),
                    _ => None,
                };
                Ok(SemanticResult::Element(element))
            }
            SemanticResult::Elements(elements) => Ok(SemanticResult::Elements(
                self.elements_allowed(elements, scope)?,
            )),
            SemanticResult::Artifact(artifact) => Ok(SemanticResult::Artifact(match artifact {
                Some(artifact) if self.artifact_allowed(&artifact, scope)? => Some(artifact),
                _ => None,
            })),
            SemanticResult::Artifacts(artifacts) => self.filter_artifacts(artifacts, scope),
            SemanticResult::ElementVectorSearch(hits) => self.filter_vectors(hits, false, scope),
            SemanticResult::ArtifactVectorSearch(hits) => self.filter_vectors(hits, true, scope),
            SemanticResult::ArtifactBlob(Some(blob)) => {
                let result = self.semantic.execute(
                    SemanticOperation::Artifact {
                        artifact_id: blob.artifact_id.clone(),
                    },
                    self.control,
                )?;
                match result {
                    SemanticResult::Artifact(Some(artifact))
                        if self.artifact_allowed(&artifact, scope)? =>
                    {
                        Ok(SemanticResult::ArtifactBlob(Some(blob)))
                    }
                    _ => Ok(SemanticResult::ArtifactBlob(None)),
                }
            }
            SemanticResult::ScopedGraph(mut graph) => {
                graph.elements = self.elements_allowed(graph.elements, scope)?;
                graph.relationships = self.visible_relationships(graph.relationships, scope)?;
                Ok(SemanticResult::ScopedGraph(graph))
            }
            SemanticResult::Relationships(edges) => Ok(SemanticResult::Relationships(
                self.visible_relationships(edges, scope)?,
            )),
            other => Ok(other),
        }
    }

    fn elements_allowed(
        &self,
        elements: Vec<SemanticElement>,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<Vec<SemanticElement>> {
        let mut allowed = Vec::new();
        for element in elements {
            if self.element_allowed(&element, scope)? {
                allowed.push(element);
            }
        }
        Ok(allowed)
    }

    fn filter_artifacts(
        &self,
        artifacts: Vec<SemanticArtifact>,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<SemanticResult> {
        let mut allowed = Vec::new();
        for artifact in artifacts {
            if self.artifact_allowed(&artifact, scope)? {
                allowed.push(artifact);
            }
        }
        Ok(SemanticResult::Artifacts(allowed))
    }
}

fn denied(root: &str) -> DbError {
    DbError::invalid_value(
        root,
        "authorized project, or owned memory through a Knowledge document export",
    )
}
