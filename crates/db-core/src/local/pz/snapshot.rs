use crate::archive::{normalized_project_root, verify_control};
use crate::{DbError, PzSnapshotResult, Result, SemanticArchive};
use std::path::Path;

/// Controlled variant used by App Core/plugin operations.
pub fn create_semantic_snapshot_controlled(
    storage: &crate::local::grafeo::storage_manager::StorageManager<'_>,
    project_root: &str,
    output_path: impl AsRef<Path>,
    control: &lumvise_resource_routing::InvocationControl,
) -> Result<PzSnapshotResult> {
    verify_control(control)?;
    let project_root = normalized_project_root(project_root)?;
    let contents = storage
        .semantic_storage()
        .pz_snapshot_data(&project_root)
        .map_err(|error| DbError::pz(crate::PzFailurePhase::Capture, error.to_string()))?;
    verify_control(control)?;
    let project_id = storage
        .project_identity(&project_root)
        .map_err(|error| DbError::pz(crate::PzFailurePhase::Capture, error.to_string()))?
        .project_id;
    SemanticArchive {
        project_id,
        snapshot_id: uuid::Uuid::now_v7().to_string(),
        canonical_remote: None,
        snapshot: contents.snapshot,
        artifact_blobs: contents.artifact_blobs,
        artifact_text_vectors: contents.artifact_text_vectors,
        element_name_vectors: contents.element_name_vectors,
    }
    .write(output_path, control)
}
