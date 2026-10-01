//! Md-file nucleus ingestion for the project knowledge root.
//!
//! When the semantic indexer upserts a markdown file element below
//! `<project>/knowledge/`, the StorageTrigger reconciliation mirrors the file
//! as a durable Knowledge artifact ("md nucleus") so projections can route by
//! `metadata.nucleus.target_path`. When the element is tombstoned, an untouched seed
//! is removed; authored database content remains durable. Artifact identity is deterministic from the md path, so
//! re-delivered batches reconcile idempotently without rewriting unchanged
//! nuclei. Database artifacts own authored Markdown after initial seeding.
//!
//! The StorageTrigger batch intentionally carries identities only, so this
//! module reads element details through the identity-validated
//! `storage.semantic` targeted-element read and never sees markdown source
//! content: nuclei carry a placeholder body and a title derived from the file
//! name.

use sha2::{Digest, Sha256};

use lumvise_plugin_sdk::{PluginContext, PluginError, StorageTriggerRequest};
use serde_json::json;

use crate::{KnowledgeArtifact, KnowledgeKind, semantic_context::SemanticElement, storage};

/// Convention: markdown files under `<project>/knowledge/` are nuclei.
pub(crate) const KNOWLEDGE_ROOT: &str = "knowledge";

const MD_NUCLEUS_TAG: &str = "md-nucleus";

/// Reconciles md-file nuclei for one StorageTrigger batch: upserted markdown
/// file elements under [`KNOWLEDGE_ROOT`] mirror into nucleus artifacts and
/// tombstoned elements remove theirs. Non-md and out-of-root elements are
/// ignored.
pub(crate) fn reconcile_for_change_batch(
    request: &StorageTriggerRequest,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let element_ids = request
        .changed
        .iter()
        .map(|change| change.entity_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if element_ids.is_empty() {
        return Ok(());
    }
    let ids = element_ids
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    for element in storage::elements_by_ids(context, &request.project_root, &ids)? {
        let Some(path) = nucleus_path(&element.path) else {
            continue;
        };
        reconcile_nucleus(context, &request.project_root, &element, &path)?;
    }
    Ok(())
}

/// Returns the md file path when it sits below the knowledge root with an
/// md/markdown extension, for example `knowledge/decisions/auth.md`.
fn nucleus_path(path: &str) -> Option<String> {
    let path = path.trim_start_matches("./");
    let rest = path.strip_prefix(KNOWLEDGE_ROOT)?.strip_prefix('/')?;
    let file_name = rest.rsplit('/').next().unwrap_or(rest);
    let extension = file_name.rsplit_once('.').map(|(_, extension)| extension)?;
    matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown").then(|| path.to_owned())
}

/// Deterministic nucleus artifact identity for one md path.
fn nucleus_artifact_id(path: &str) -> String {
    format!(
        "knowledge-nucleus-{}",
        hex::encode(Sha256::digest(path.as_bytes()))
    )
}

/// Seeds an indexed Markdown identity once. Subsequent file events cannot
/// replace Markdown, links, or metadata authored through the Knowledge API.
fn reconcile_nucleus(
    context: &mut PluginContext<'_>,
    project_root: &str,
    element: &SemanticElement,
    path: &str,
) -> Result<(), PluginError> {
    let seed = nucleus_seed(project_root, element, path);
    let existing = storage::get_knowledge(context, &seed.artifact_id)?;
    if element.lifecycle != "active" {
        if existing.as_ref() == Some(&seed) {
            storage::remove_knowledge(context, &seed.artifact_id)?;
        }
        return Ok(());
    }
    if existing.is_some() {
        return Ok(());
    }
    storage::put_knowledge(context, &seed)
}

fn nucleus_seed(project_root: &str, element: &SemanticElement, path: &str) -> KnowledgeArtifact {
    KnowledgeArtifact {
        artifact_id: nucleus_artifact_id(path),
        semantic_element_id: element.semantic_element_id.clone(),
        knowledge_type: KnowledgeKind::Definition,
        title: nucleus_title(element),
        content: nucleus_content(path, &element.semantic_element_id),
        tags: vec!["nucleus".into(), MD_NUCLEUS_TAG.into()],
        dependencies: vec![],
        metadata: json!({"nucleus": {"id": "md-nucleus", "artifact_role": "nucleus",
            "status": "current", "source": "md-file-ingestion",
            "target_path": path, "target_element_id": element.semantic_element_id,
            "project_root": project_root}}),
        path: Some(path.to_owned()),
        project_root: Some(project_root.to_owned()),
    }
}

/// Titles the nucleus from the file name without its md extension; the trigger
/// payload never carries markdown source, so no heading text is available here.
fn nucleus_title(element: &SemanticElement) -> String {
    let name = if element.name.trim().is_empty() {
        element
            .path
            .rsplit('/')
            .find(|part| !part.is_empty())
            .unwrap_or("Knowledge file")
    } else {
        element.name.as_str()
    };
    match name.rsplit_once('.') {
        Some((stem, extension))
            if matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown")
                && !stem.is_empty() =>
        {
            stem.to_owned()
        }
        _ => name.to_owned(),
    }
}

/// Placeholder body: the StorageTrigger batch carries identities only, so the
/// markdown source content is not copied into the nucleus.
fn nucleus_content(path: &str, semantic_element_id: &str) -> String {
    format!(
        "Knowledge nucleus for the markdown file `{path}`.\n\n\
         The file is indexed as semantic element `{semantic_element_id}`. The storage \
         trigger payload carries identities only, so the markdown source content is not \
         mirrored into this nucleus; it tracks the file identity and knowledge path."
    )
}
