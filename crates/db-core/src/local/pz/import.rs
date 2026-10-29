//! PZ validation precedes the semantic owner's atomic archive activation.
use crate::local::grafeo::storage_manager::StorageManager;
use crate::{PzImportResult, Result, SemanticArchive};
use lumvise_resource_routing::InvocationControl;
use std::path::Path;

/// Restores compatible revision history through the existing graph/SQL publication fence.
/// Example: `import_semantic_snapshot_controlled(&storage, "/repo", "project.pz", &control)?`.
pub fn import_semantic_snapshot_controlled(
    storage: &StorageManager<'_>,
    project_root: &str,
    input_path: impl AsRef<Path>,
    control: &InvocationControl,
) -> Result<PzImportResult> {
    let archive = SemanticArchive::read(input_path, project_root, control)?;
    storage
        .semantic_storage()
        .with_control(control.clone())
        .restore_semantic_archive(&archive)
}
