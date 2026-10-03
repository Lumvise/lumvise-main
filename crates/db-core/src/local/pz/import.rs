//! PZ archive import into one local project root.
//!
//! The inverse of `create_semantic_snapshot_controlled`: the archive passes the
//! same full validation `PzArchive::open` performs, then its rows are written
//! through the live storage paths so publication, change hooks, and blob
//! policy stay authoritative.

use crate::PzImportResult;
use crate::archive::verify_control;
use crate::local::grafeo::storage_manager::StorageManager;
use crate::{Result, SemanticArchive};
use lumvise_resource_routing::InvocationControl;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Imports one validated PZ archive into `project_root`.
///
/// Structure replaces the project's published structure. Archive artifacts
/// whose IDs the database lacks are inserted with their blob content, their
/// attachment blobs (canvas images, under their original refs), and their text
/// vector; existing artifact IDs stay authoritative. External references are
/// not imported because their foreign targets live in other projects.
pub fn import_semantic_snapshot_controlled(
    storage: &StorageManager<'_>,
    project_root: &str,
    input_path: impl AsRef<Path>,
    control: &InvocationControl,
) -> Result<PzImportResult> {
    let archive = SemanticArchive::read(input_path, project_root, control)?;
    let project_root = &archive.snapshot.project_root;
    let blobs = archive
        .artifact_blobs
        .iter()
        .map(|blob| (blob.content_ref.as_str(), blob))
        .collect::<HashMap<_, _>>();
    let artifact_vectors = archive
        .artifact_text_vectors
        .iter()
        .map(|stored| (stored.artifact_id.as_str(), stored))
        .collect::<HashMap<_, _>>();
    let semantic = storage.semantic_storage().with_control(control.clone());
    let structure = semantic.sync_semantic_structure(
        project_root,
        &archive.snapshot.elements,
        &archive.snapshot.relationships,
    )?;
    semantic.store_element_name_vectors(project_root, &archive.element_name_vectors)?;

    let archived_ids = archive
        .snapshot
        .artifacts
        .iter()
        .map(|artifact| artifact.artifact_id.clone())
        .collect::<HashSet<_>>();
    let existing_ids = semantic
        .artifacts_by_ids(&archived_ids)?
        .into_iter()
        .map(|artifact| artifact.artifact_id)
        .collect::<HashSet<_>>();
    let mut artifacts_imported = 0;
    for artifact in &archive.snapshot.artifacts {
        if existing_ids.contains(&artifact.artifact_id) {
            continue;
        }
        verify_control(control)?;
        let vector = artifact_vectors
            .get(artifact.artifact_id.as_str())
            .map(|stored| (stored.source_text.as_str(), &stored.vector));
        let blob = artifact
            .content_ref
            .as_ref()
            .and_then(|content_ref| blobs.get(content_ref.as_str()));
        match blob {
            Some(blob) => {
                let staged = semantic.artifact_with_content_policy(
                    artifact,
                    &blob.media_type,
                    &blob.content,
                )?;
                semantic.commit_staged_artifact_update(&staged, vector)?;
            }
            None => {
                let mut artifact = artifact.clone();
                artifact.content_ref = None;
                semantic.commit_artifact_update(&artifact, vector)?;
            }
        }
        // Attachments (canvas images) keep their original refs: scenes
        // reference them by content ref.
        let mut attachments = blobs
            .values()
            .filter(|attachment| {
                attachment.artifact_id == artifact.artifact_id
                    && artifact.content_ref.as_ref() != Some(&attachment.content_ref)
            })
            .collect::<Vec<_>>();
        attachments.sort_by(|left, right| left.content_ref.cmp(&right.content_ref));
        for attachment in attachments {
            semantic.put_owned_blob(attachment)?;
        }
        artifacts_imported += 1;
    }

    Ok(PzImportResult {
        project_id: archive.project_id,
        snapshot_id: archive.snapshot_id,
        canonical_remote: archive.canonical_remote,
        structure,
        artifacts_imported,
        artifacts_kept: archive.snapshot.artifacts.len() - artifacts_imported,
    })
}
