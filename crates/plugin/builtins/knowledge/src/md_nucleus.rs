//! Md-file nucleus ingestion for the project knowledge root.
//!
//! Indexed Markdown files below `<project>/knowledge/` seed independent Context documents.
//! Source events carry identities, so the initial body is a link rather than a copy of file bytes.
//! Database-authored content and deleted document identities survive subsequent source events.

use sha2::{Digest, Sha256};

use lumvise_plugin_sdk::{PluginContext, PluginError, StorageTriggerRequest};
use serde_json::json;

use crate::{
    documents::{self, DocumentGenerator},
    semantic_context::SemanticElement,
    storage,
};
use lumvise_contracts::{
    CreateKnowledgeDocumentRequest, KnowledgeDocument, KnowledgeDocumentKind,
    KnowledgeDocumentReference,
};

/// Convention: markdown files under `<project>/knowledge/` are nuclei.
pub(crate) const KNOWLEDGE_ROOT: &str = "knowledge";

const MD_NUCLEUS_TAG: &str = "md-nucleus";

/// Reconciles md-file nuclei for one StorageTrigger batch: upserted markdown
/// file elements under [`KNOWLEDGE_ROOT`] seed source Context documents. Source
/// removal retains independently owned bodies. Non-md and out-of-root elements are ignored.
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
        if element.element_kind != "file" {
            continue;
        }
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

/// Project-qualified identity avoids collisions for the same Markdown path in different projects.
fn nucleus_document_id(project_root: &str, path: &str) -> String {
    format!(
        "knowledge-nucleus-{}",
        hex::encode(Sha256::digest(format!("{project_root}\0{path}").as_bytes()))
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
    if element.lifecycle != "active" {
        return Ok(());
    }
    let known = documents::known_source_documents(context, project_root)?;
    if known.iter().any(|document| {
        document.document_kind == KnowledgeDocumentKind::Context
            && document.tags.iter().any(|tag| tag == MD_NUCLEUS_TAG)
            && document.metadata["nucleus"]["target_path"] == path
    }) {
        return Ok(());
    }
    if documents::conversion_required_for_source(
        context,
        project_root,
        &element.semantic_element_id,
        MD_NUCLEUS_TAG,
    )? {
        return Err(PluginError::new(
            "knowledge_conversion_required",
            format!(
                "Markdown source `{}` in `{project_root}` still has a retired nucleus artifact; preview and apply document conversion before seeding",
                element.semantic_element_id
            ),
            false,
        ));
    }
    let seed = nucleus_seed(project_root, element, path);
    match documents::save_generated(
        context,
        &DocumentGenerator::MarkdownSeed.owner(&element.semantic_element_id),
        seed,
        None,
    ) {
        Ok(_) => Ok(()),
        Err(error) if error.code == "generated_document_deleted" => Ok(()),
        Err(error) => Err(error),
    }
}

fn nucleus_seed(project_root: &str, element: &SemanticElement, path: &str) -> KnowledgeDocument {
    documents::draft_document(CreateKnowledgeDocumentRequest {
        document_id: nucleus_document_id(project_root, path),
        project_root: project_root.into(),
        document_kind: KnowledgeDocumentKind::Context,
        title: nucleus_title(element),
        content: nucleus_content(path, &element.semantic_element_id),
        tags: vec!["nucleus".into(), MD_NUCLEUS_TAG.into()],
        path: Some(path.into()),
        references: vec![KnowledgeDocumentReference::SemanticElement {
            semantic_element_id: element.semantic_element_id.clone(),
        }],
        metadata: json!({"nucleus":{"id":"md-nucleus","document_role":"source_context","status":"current","source":"md-file-ingestion",
            "target_path":path,"target_element_id":element.semantic_element_id,"project_root":project_root}}),
    })
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
