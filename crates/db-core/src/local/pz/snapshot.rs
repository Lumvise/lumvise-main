use crate::archive::verify_control;
use crate::{DbError, PzSnapshotResult, Result};
use std::path::Path;

/// Controlled variant used by App Core/plugin operations.
pub fn create_semantic_snapshot_controlled(
    storage: &crate::local::grafeo::storage_manager::StorageManager<'_>,
    project_root: &str,
    output_path: impl AsRef<Path>,
    control: &lumvise_resource_routing::InvocationControl,
) -> Result<PzSnapshotResult> {
    verify_control(control)?;
    crate::local::project_archive::capture(storage, project_root, control)
        .map_err(|error| DbError::pz(crate::PzFailurePhase::Capture, error.to_string()))?
        .write(output_path, control)
}
