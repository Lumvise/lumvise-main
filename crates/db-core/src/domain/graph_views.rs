//! Shared graph-view rules; snapshot methods are the public entrypoint.
mod file_projection;
mod general_projection;
mod media_artifacts;
mod projection_records;
mod scoped;
mod snapshot_source;
use crate::{
    DbError, Result, ScopedSemanticRead, SemanticArtifact, SemanticElement,
    SemanticGraphArtifactPreview, SemanticGraphEdge, SemanticGraphGranularity, SemanticGraphNode,
    SemanticGraphProjection, SemanticGraphProjectionRequest, SemanticProjectSnapshot,
    SemanticRelationship, SemanticScopedGraph,
};
pub(crate) use file_projection::slice_file_projection;
use file_projection::*;
use general_projection::*;
#[cfg(test)]
pub(crate) use media_artifacts::media_kind;
pub(crate) use media_artifacts::{MediaAssociationSource, artifact_inheritance_from_source};
pub(crate) use projection_records::{BatchedGraphRows, ProjectionArtifact, ProjectionElement};
pub(crate) use scoped::{GraphViewSource, scoped_from_source};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
impl SemanticProjectSnapshot {
    /// Builds a complete renderer graph using the same rules as local persistence.
    /// Example: `snapshot.renderer_graph(&request)?`.
    pub fn renderer_graph(
        &self,
        request: &SemanticGraphProjectionRequest,
    ) -> Result<SemanticGraphProjection> {
        validate_root(&self.project_root, &request.project_root)?;
        if request.granularity != SemanticGraphGranularity::File {
            return project_projection(
                BatchedGraphRows::snapshot(self),
                request,
                self.commit_version,
                self.published_at.clone(),
            );
        }
        let canonical = SemanticGraphProjectionRequest {
            target_path: None,
            recursive: true,
            include_external: true,
            include_first_neighbors: false,
            ..request.clone()
        };
        let projection = project_projection(
            BatchedGraphRows::snapshot(self),
            &canonical,
            self.commit_version,
            self.published_at.clone(),
        )?;
        Ok(slice_file_projection(&projection, request))
    }
    /// Reads direct artifacts, or an unambiguous media association, without copying records.
    /// Example: `snapshot.artifacts_with_inheritance("image:hero")?`.
    pub fn artifacts_with_inheritance(&self, id: &str) -> Result<Vec<SemanticArtifact>> {
        artifact_inheritance_from_source(self, id)
    }
    /// Selects a bounded structural/location/dependency view from this publication.
    /// Example: `snapshot.scoped_graph(&request)?`.
    pub fn scoped_graph(&self, request: &ScopedSemanticRead) -> Result<SemanticScopedGraph> {
        scoped_from_source(
            self,
            request,
            self.commit_version,
            self.published_at.clone(),
        )
    }
}
fn validate_root(actual: &str, requested: &str) -> Result<()> {
    if actual != requested || requested.trim().is_empty() {
        return Err(DbError::invalid_value(
            requested,
            "request project root matching snapshot project root",
        ));
    }
    Ok(())
}
pub(crate) fn project_projection(
    rows: BatchedGraphRows,
    request: &SemanticGraphProjectionRequest,
    revision: i64,
    published_at: String,
) -> Result<SemanticGraphProjection> {
    if request.granularity == SemanticGraphGranularity::File {
        return project_file(rows, request, revision, published_at);
    }
    project_general(rows, request, revision, published_at)
}
