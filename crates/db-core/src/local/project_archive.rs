//! Captures complete project records for portable transfers and the PZ writer.
//! LocalPersistence owns the public operation; storage leases remain internal.
use crate::local::grafeo::storage_manager::StorageManager;
use crate::{Result, SemanticArchive};
use lumvise_resource_routing::InvocationControl;

pub(super) fn capture(
    storage: &StorageManager<'_>,
    project_root: &str,
    control: &InvocationControl,
) -> Result<SemanticArchive> {
    super::persistence::verify_control(control)?;
    let project_root = crate::archive::normalized_project_root(project_root)?;
    let project_id = storage.project_identity(&project_root)?.project_id;
    let contents = storage.semantic_storage().pz_snapshot_data(&project_root)?;
    super::persistence::verify_control(control)?;
    Ok(SemanticArchive {
        project_id,
        snapshot_id: uuid::Uuid::now_v7().to_string(),
        canonical_remote: None,
        snapshot: contents.snapshot,
        artifact_blobs: contents.artifact_blobs,
        artifact_text_vectors: contents.artifact_text_vectors,
        element_name_vectors: contents.element_name_vectors,
    })
}
