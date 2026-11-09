//! Checks the immutable MCP origin before owner calls, including nested Knowledge writes.
use super::*;
use lumvise_db_core::{SemanticOperation, SemanticPersistence};
use lumvise_plugin_runtime::PluginInvocationContext;

impl KnowledgeAuthority {
    pub(in crate::plugin) fn authorize_connector_request(
        &self,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        plugin: &str,
        export: &str,
        input: &Value,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        if context.mcp_origin().is_none() || plugin != "builtin.knowledge" {
            return Ok(());
        }
        let entered = context.clone().enter_plugin(plugin, export);
        self.authorize_connector_call(semantic, &entered, control)?;
        if let Some(root) = input["project_root"]
            .as_str()
            .filter(|_| write_operation(export).is_some())
        {
            let scope = self.current(semantic, &entered, control)?;
            self.require_connector_target(semantic, &entered, root, &scope, control)?;
        }
        Ok(())
    }

    pub(in crate::plugin) fn require_connector_target(
        &self,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        root: &str,
        destination: &lumvise_contracts::KnowledgeAccessContext,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        if context.mcp_origin().is_none() {
            return Ok(());
        }
        let project = self.connector_project(semantic, context, control)?;
        let ordinary = destination.project_grants.iter().any(|grant| {
            grant.project.project_root == root && grant.project.same_project(&project)
        });
        let private = destination
            .space
            .as_ref()
            .is_some_and(|space| space.project_root == root)
            && destination
                .active_project
                .iter()
                .chain(destination.source_projects.iter())
                .any(|source| source.same_project(&project))
            && destination
                .active_project
                .iter()
                .chain(destination.source_projects.iter())
                .all(|source| source.same_project(&project));
        if ordinary || private {
            return Ok(());
        }
        Err(unsupported(
            "write outside the connector's current project lineage",
        ))
    }

    pub(in crate::plugin) fn authorize_connector_call(
        &self,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        if context.mcp_origin().is_none() {
            return Ok(());
        }
        let Some(export) = knowledge_export(context) else {
            return Ok(());
        };
        let operation = match write_operation(export) {
            Some(operation) => operation,
            None if read_export(export) => return Ok(()),
            None => return Err(unsupported(export)),
        };
        self.authorize_connector_operation(semantic, context, operation, control)
    }

    fn authorize_connector_operation(
        &self,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        operation: ConnectorKnowledgeOperation,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        let origin = context.mcp_origin().expect("checked MCP origin");
        let id = origin
            .connector_id
            .as_deref()
            .ok_or_else(|| unsupported("unidentified MCP connector"))?;
        self.mcp_membership
            .require_connector(&origin.connection_id, Some(id))
            .map_err(|error| invalid(&json!(origin.connection_id), &error))?;
        let project = self.connector_project(semantic, context, control)?;
        let destination = self.current(semantic, context, control)?;
        if destination
            .source_projects
            .iter()
            .chain(destination.active_project.iter())
            .any(|p| !p.same_project(&project))
        {
            return Err(unsupported(
                "a different project or aggregated private scope",
            ));
        }
        self.require_connector_write(id, &project, operation, control)
    }

    fn connector_project(
        &self,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<KnowledgeProjectIdentity, HostCapabilityError> {
        let origin = context
            .mcp_origin()
            .ok_or_else(|| unsupported("missing MCP origin"))?;
        let scope = context
            .clone()
            .with_route_identity(
                &origin.connection_id,
                Some(lumvise_contracts::KNOWLEDGE_MCP_SCOPE.into()),
            )
            .with_scope_epoch(origin.project_epoch);
        let original = self.current_mcp(semantic, &scope, control)?;
        original
            .active_project
            .ok_or_else(|| unsupported("unbound MCP project"))
    }

    pub(in crate::plugin) fn authorize_connector_semantic(
        &self,
        context: &PluginInvocationContext,
        operation: &SemanticOperation,
    ) -> Result<(), HostCapabilityError> {
        // A Knowledge/Agent ancestor does not own MCP section-vector staging.
        if context.mcp_origin().is_some() && text_generation_mutation(operation) {
            return if search_index_rebuild_vectors(context, operation) {
                Ok(())
            } else {
                Err(unsupported(
                    "section-vector staging outside the innermost search rebuild",
                ))
            };
        }
        if context.mcp_origin().is_none()
            || knowledge_export(context).is_some()
            || super::super::agent::agent_frame(context).is_some()
            || !semantic_mutation(operation)
            // Owner repair keeps its own direct-export, bound-owner check in `authorize_recovery`.
            || matches!(operation, SemanticOperation::RecoverRevisionIntegrity(_))
            || search_index_rebuild_vectors(context, operation)
        {
            return Ok(());
        }
        Err(unsupported(
            "raw Semantic mutation; use the app-owned source scanner or a Knowledge create/update export",
        ))
    }

    pub(in crate::plugin) fn authorize_connector_index(
        &self,
        context: &PluginInvocationContext,
    ) -> Result<(), HostCapabilityError> {
        if context.mcp_origin().is_none()
            || knowledge_export(context).is_some()
            || super::super::agent::agent_frame(context).is_some()
            || context.plugin_chain().iter().any(|frame| {
                frame.plugin_id == "builtin.semantic" && frame.export_id == "index_project"
            })
        {
            return Ok(());
        }
        Err(unsupported(
            "raw index publication; use the app-owned index_project source scanner",
        ))
    }
}

fn text_generation_mutation(operation: &SemanticOperation) -> bool {
    matches!(
        operation,
        SemanticOperation::BeginElementTextVectorGeneration { .. }
            | SemanticOperation::StageElementTextVectorChunk { .. }
            | SemanticOperation::CommitElementTextVectorGeneration { .. }
            | SemanticOperation::AbortElementTextVectorGeneration { .. }
    )
}

/// Vectors the Semantic search index rebuild derives from stored names and page text
/// (ADR 0002, 2026-11-07): the caller passes only a project root, so it cannot supply content.
fn search_index_rebuild_vectors(
    context: &PluginInvocationContext,
    operation: &SemanticOperation,
) -> bool {
    matches!(
        operation,
        SemanticOperation::StoreElementNameVectors { .. }
            | SemanticOperation::StoreElementTextVectors { .. }
            | SemanticOperation::BeginElementTextVectorGeneration { .. }
            | SemanticOperation::StageElementTextVectorChunk { .. }
            | SemanticOperation::CommitElementTextVectorGeneration { .. }
            | SemanticOperation::AbortElementTextVectorGeneration { .. }
    ) && context.plugin_chain().last().is_some_and(|frame| {
        frame.plugin_id == "builtin.semantic" && frame.export_id == "rebuild_search_index"
    })
}

fn write_operation(export: &str) -> Option<ConnectorKnowledgeOperation> {
    match export {
        "create_document" | "document.create" | "create_knowledge" | "knowledge.create" => {
            Some(ConnectorKnowledgeOperation::Create)
        }
        "update_document" | "document.update" | "update_knowledge" | "knowledge.update" => {
            Some(ConnectorKnowledgeOperation::Update)
        }
        _ => None,
    }
}

fn knowledge_export(context: &PluginInvocationContext) -> Option<&str> {
    context
        .plugin_chain()
        .iter()
        .rev()
        .find(|frame| frame.plugin_id == "builtin.knowledge")
        .map(|frame| frame.export_id.as_str())
}

fn read_export(export: &str) -> bool {
    matches!(
        export,
        "get_document"
            | "document.get"
            | "list_documents"
            | "document.list"
            | "search_documents"
            | "document.search"
            | "resolve_document_space"
            | "document.space"
            | "resolve_document_reference"
            | "document.reference"
            | "inspect_document_space"
            | "get_document_write_receipt"
            | "get_document_hook"
            | "document.get_hook"
            | "get_document_citation_retention"
            | "get_knowledge"
            | "search_knowledge"
            | "list_knowledge_for_element"
            | "knowledge.get"
            | "knowledge.search"
            | "knowledge.list_for_element"
            | "knowledge.list_dependents"
            | "knowledge.find_elements"
            | "knowledge.get_element"
            | "list_knowledge"
            | "list_knowledge_dependents"
            | "find_semantic_elements"
            | "get_semantic_element"
            | "preview_knowledge_transfer"
            | "project_knowledge"
            | "get_cultivation_run"
            | "debug_c4_nucleus"
            | "manifest"
            | "http.knowledge.manifest"
            | "http.knowledge.export"
            | "http.knowledge.page"
            | "http.knowledge.c4_debug"
            | "http.knowledge.resolve_target"
            | "http.knowledge.events"
            | "http.knowledge.projection_artifacts"
            | "get_artifact_write_receipt"
    )
}

fn semantic_mutation(operation: &SemanticOperation) -> bool {
    use SemanticOperation::*;
    !matches!(
        operation,
        RevisionFamilies(_)
            | RevisionConnections(_)
            | RevisionSelection { .. }
            | RevisionSelectionMembers(_)
            | RevisionSelectionAt { .. }
            | RevisionSourceAnchor { .. }
            | ResolveRevision { .. }
            | RevisionHistory { .. }
            | Element { .. }
            | ElementsByIds { .. }
            | ElementsByIdsIncludingInactive { .. }
            | ArtifactsByIds { .. }
            | SearchElementCandidates { .. }
            | SearchElementNameVectors { .. }
            | SearchElementTextVectors { .. }
            | SearchArtifactTextVectors { .. }
            | RelationshipsFrom { .. }
            | RelationshipsTouchingElements { .. }
            | Artifact { .. }
            | ArtifactBlobGet { .. }
            | ArtifactsForElements { .. }
            | ArtifactsForElementWithInheritance { .. }
            | ArtifactDependents { .. }
            | ProjectRoots
            | ProjectIdentity { .. }
            | ProjectAccess { .. }
            | SemanticRevision
            | ProjectArtifacts { .. }
            | ProjectSnapshot { .. }
            | SelectiveSubgraph { .. }
            | ProjectRendererGraph(_)
            | ChangesSinceRevision { .. }
            | ChangeHookRegistrations
            | ChangeHookDirtyBatch { .. }
            | WaitForSemanticRevision { .. }
            | CandidateSourceElements { .. }
            | ScopedRead(_)
            | ProjectElementCounts { .. }
            | ProjectArchive { .. }
            | SearchProjectStructure { .. }
            | UnavailableRevisions { .. }
            | InspectRevisionIntegrity { .. }
    )
}

fn unsupported(value: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        "runtime.knowledge_scope",
        "connector_write_denied",
        format!(
            "connector operation {value:?}; expected an authenticated project-scoped Knowledge create/update grant"
        ),
        false,
    )
}
