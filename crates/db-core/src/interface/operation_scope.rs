//! Portable routing facts. Adapters authorize this root; ID-only requests still need their own lookup.
use super::{ProjectSnapshotScope, ScopedSemanticRead, SemanticOperation};

impl SemanticOperation {
    /// Returns the explicitly declared project route, never an inferred record owner.
    /// Example: an online PublishRevisions stays online through `operation.project_root()`.
    pub fn project_root(&self) -> Option<&str> {
        revision_root(self)
            .or_else(|| publication_root(self))
            .or_else(|| view_root(self))
            .or_else(|| selector_root(self))
    }
}

fn revision_root(operation: &SemanticOperation) -> Option<&str> {
    use SemanticOperation::*;
    match operation {
        RevisionSelection { project_root, .. }
        | RevisionSelectionAt { project_root, .. }
        | RevisionSourceAnchor { project_root, .. }
        | ResolveRevision { project_root, .. }
        | RevisionHistory { project_root }
        | PlanRevisionRetention { project_root, .. } => Some(project_root),
        RevisionConnections(query) => Some(&query.project_root),
        RevisionFamilies(query) => Some(&query.project_root),
        RevisionSelectionMembers(query) => Some(&query.project_root),
        PublishRevisions(request) => Some(&request.project_root),
        SelectRevisions(request) => Some(&request.project_root),
        AcquireCitationRetention(request) => Some(&request.project_root),
        ReleaseCitationRetention(request) => Some(&request.project_root),
        CollectRevisionRetention { plan, .. } => Some(&plan.project_root),
        _ => None,
    }
}

fn publication_root(operation: &SemanticOperation) -> Option<&str> {
    use SemanticOperation::*;
    match operation {
        CreatePzSnapshot { project_root, .. }
        | ImportPzSnapshot { project_root, .. }
        | SyncStructure { project_root, .. }
        | BeginProjectSnapshot { project_root, .. }
        | StoreElementNameVectors { project_root, .. }
        | StoreArtifactTextVectors { project_root, .. } => Some(project_root),
        SyncPartition { partition, .. } => Some(&partition.project_root),
        _ => None,
    }
}

fn view_root(operation: &SemanticOperation) -> Option<&str> {
    use SemanticOperation::*;
    match operation {
        ElementsByIds { project_root, .. }
        | ElementsByIdsIncludingInactive { project_root, .. }
        | SearchElementNameVectors { project_root, .. }
        | SearchArtifactTextVectors { project_root, .. }
        | ProjectIdentity { project_root }
        | ProjectAccess { project_root }
        | ProjectArtifacts { project_root, .. }
        | SelectiveSubgraph { project_root, .. }
        | ProjectElementCounts { project_root }
        | ProjectArchive { project_root } => Some(project_root),
        SearchElementCandidates { project_root, .. } => project_root.as_deref(),
        _ => None,
    }
}

fn selector_root(operation: &SemanticOperation) -> Option<&str> {
    use SemanticOperation::*;
    match operation {
        ProjectSnapshot { scope, .. } | ScopedRead(ScopedSemanticRead::Structure { scope, .. }) => {
            scope_root(scope)
        }
        ScopedRead(
            ScopedSemanticRead::Location { project_root, .. }
            | ScopedSemanticRead::PathPrefix { project_root, .. },
        ) => Some(project_root),
        ProjectRendererGraph(request) => Some(&request.project_root),
        ChangesSinceRevision { scope, .. } | RegisterChangeHook { scope, .. } => {
            Some(&scope.project_root)
        }
        _ => None,
    }
}

fn scope_root(scope: &ProjectSnapshotScope) -> Option<&str> {
    match scope {
        ProjectSnapshotScope::ProjectRoot(root) => Some(root),
        ProjectSnapshotScope::SemanticElement(_) => None,
    }
}
