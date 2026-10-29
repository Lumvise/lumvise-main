//! Automatic content uses the same document records and revisions as authored edits.
//! Source identity and the last generated signature prevent taking over another body.

use chrono::Utc;
use lumvise_contracts::{KnowledgeDocument, KnowledgeDocumentIdentity, KnowledgeDocumentKind};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::records::{self, DocumentState};

#[derive(Clone, Copy, Debug)]
pub(crate) enum DocumentGenerator {
    FunctionalContext,
    C4Report,
    MarkdownSeed,
    MemoryHook,
}

impl DocumentGenerator {
    pub(crate) fn owner(self, source_element_id: &str) -> DocumentGenerationOwner {
        DocumentGenerationOwner {
            generator: self,
            source_element_id: source_element_id.into(),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::FunctionalContext => "knowledge.functional",
            Self::C4Report => "knowledge.c4",
            Self::MarkdownSeed => "knowledge.markdown_seed",
            Self::MemoryHook => "knowledge.memory_hook",
        }
    }

    fn kind(self) -> KnowledgeDocumentKind {
        match self {
            Self::C4Report => KnowledgeDocumentKind::Report,
            _ => KnowledgeDocumentKind::Context,
        }
    }

    fn accepts_kind(self, kind: KnowledgeDocumentKind) -> bool {
        match self {
            Self::MemoryHook => matches!(
                kind,
                KnowledgeDocumentKind::Context | KnowledgeDocumentKind::Report
            ),
            _ => kind == self.kind(),
        }
    }
}

pub(crate) struct DocumentGenerationOwner {
    generator: DocumentGenerator,
    source_element_id: String,
}

/// Exact document base captured with a generation input, independent of later selection changes.
/// Example: `current.as_ref().map(ObservedDocumentRevision::from)` before preparing a proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObservedDocumentRevision {
    pub(crate) semantic_element_id: String,
    pub(crate) revision: u64,
}

impl From<&KnowledgeDocument> for ObservedDocumentRevision {
    fn from(document: &KnowledgeDocument) -> Self {
        Self {
            semantic_element_id: document.semantic_element_id.clone(),
            revision: document.lifecycle.revision,
        }
    }
}

/// These conflicts require review rather than another automatic overwrite attempt.
/// Example: is_generation_hold(error) moves a completed job into its existing failure state.
pub(crate) fn is_generation_hold(error: &PluginError) -> bool {
    matches!(
        error.code.as_str(),
        "generated_document_authored"
            | "generated_document_deleted"
            | "generated_document_inactive"
            | "document_exists"
            | "document_revision_conflict"
            | "knowledge_revision_conflict"
            | "knowledge_project_mismatch"
    )
}

/// Publish one generated proposal through the canonical owner, preserving authored changes.
/// Example: save(context, &DocumentGenerator::C4Report.owner(source_id), proposal, Some(read_revision)).
pub(crate) fn save(
    context: &mut PluginContext<'_>,
    owner: &DocumentGenerationOwner,
    proposal: KnowledgeDocument,
    expected_revision: Option<ObservedDocumentRevision>,
) -> Result<KnowledgeDocument, PluginError> {
    validate(owner, &proposal)?;
    let request = json!({"owner":owner.generator.name(), "source_element_id":owner.source_element_id,
        "proposal":proposal, "expected_revision":expected_revision});
    let receipt = crate::publication_journal::execute(
        context,
        "document.generate",
        &request,
        None,
        |context, operation_id| {
            let identity = KnowledgeDocumentIdentity {
                project_root: proposal.project_root.clone(),
                document_id: proposal.document_id.clone(),
            };
            let current = observe_generation_document(context, &identity)?;
            let mut prepared = prepare(
                owner,
                proposal.clone(),
                current.as_ref(),
                expected_revision.as_ref(),
                context,
            )?;
            if current.as_ref() != Some(&prepared) {
                crate::lifecycle::stamp(
                    &mut prepared.lifecycle,
                    current.as_ref().map(|document| &document.lifecycle),
                    Utc::now(),
                );
            }
            let expected_locator = current
                .as_ref()
                .map(|document| document.semantic_element_id.clone());
            let resources = match current {
                Some(_) => super::assets::for_update(context, &prepared)?,
                None => {
                    refuse_global_collision(context, &prepared)?;
                    super::assets::new_resource(context, &mut prepared)?
                }
            };
            super::publication::prepare(
                context,
                &prepared,
                resources.into_iter().collect(),
                expected_locator.as_deref(),
                operation_id,
            )
            .map(Some)
        },
    )?;
    super::publication::accepted(
        context,
        &proposal.project_root,
        &proposal.document_id,
        receipt.as_ref(),
    )
}

fn prepare(
    owner: &DocumentGenerationOwner,
    mut proposal: KnowledgeDocument,
    current: Option<&KnowledgeDocument>,
    expected: Option<&ObservedDocumentRevision>,
    context: &mut PluginContext<'_>,
) -> Result<KnowledgeDocument, PluginError> {
    let metadata_fields = managed_fields(&proposal);
    validate_expected(&proposal, current, expected)?;
    if let Some(current) = current {
        validate_ownership(owner, current, context)?;
        preserve_authored_context(&mut proposal, current)?;
    }
    proposal.metadata["generation"] = marker(owner, &proposal, &metadata_fields);
    Ok(proposal)
}

/// Captures the selected generated-document base before external work, refusing deleted intent.
/// Example: `observe_generation_document(context, &identity)` before submitting a job.
pub(crate) fn observe_generation_document(
    context: &mut PluginContext<'_>,
    identity: &KnowledgeDocumentIdentity,
) -> Result<Option<KnowledgeDocument>, PluginError> {
    match records::scoped_state(context, identity)? {
        DocumentState::Stored(document) => Ok(Some(document)),
        DocumentState::Missing => Ok(None),
        DocumentState::Deleted(_) => Err(super::invalid(
            "generated_document_deleted",
            &identity.document_id,
            "a retained document; automatic work cannot recreate a deleted body",
        )),
    }
}

fn validate(
    owner: &DocumentGenerationOwner,
    proposal: &KnowledgeDocument,
) -> Result<(), PluginError> {
    for (field, value) in [
        ("document_id", proposal.document_id.as_str()),
        ("project_root", proposal.project_root.as_str()),
        ("title", proposal.title.as_str()),
        ("generation source", owner.source_element_id.as_str()),
    ] {
        crate::artifact::require(value, field)?;
    }
    records::validate_references(&proposal.references)?;
    if !owner.generator.accepts_kind(proposal.document_kind) || !proposal.metadata.is_object() {
        return Err(super::invalid(
            "invalid_generated_document",
            &format!(
                "{} ({:?}, {})",
                proposal.document_id, proposal.document_kind, proposal.metadata
            ),
            &format!(
                "a {:?} document with object metadata",
                owner.generator.kind()
            ),
        ));
    }
    Ok(())
}

fn validate_expected(
    proposal: &KnowledgeDocument,
    current: Option<&KnowledgeDocument>,
    expected: Option<&ObservedDocumentRevision>,
) -> Result<(), PluginError> {
    if current.map(ObservedDocumentRevision::from).as_ref() == expected {
        return Ok(());
    }
    Err(super::invalid(
        "document_revision_conflict",
        &format!("{} at expected revision {expected:?}", proposal.document_id),
        &format!(
            "the read revision {:?}",
            current.map(ObservedDocumentRevision::from)
        ),
    ))
}

fn validate_ownership(
    owner: &DocumentGenerationOwner,
    current: &KnowledgeDocument,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let stored = &current.metadata["generation"];
    if !owner.generator.accepts_kind(current.document_kind)
        || !crate::lifecycle::is_active(&current.lifecycle, Utc::now())
    {
        return Err(super::invalid(
            "generated_document_inactive",
            &format!(
                "{} ({:?}, {:?}, expires {:?})",
                current.document_id,
                current.document_kind,
                current.lifecycle.status,
                current.lifecycle.expires_at
            ),
            "an active document of this workflow",
        ));
    }
    let fields: Option<Vec<String>> = stored["metadata_fields"].as_array().and_then(|fields| {
        fields
            .iter()
            .map(|field| field.as_str().map(str::to_owned))
            .collect()
    });
    if stored["owner"] != owner.generator.name()
        || !same_selected_source(owner, current, context)?
        || fields
            .as_ref()
            .is_none_or(|fields| stored["content_sha256"] != signature(current, fields))
    {
        return Err(super::invalid(
            "generated_document_authored",
            &current.document_id,
            &format!(
                "unchanged generated content owned by {} for {}; preserve the authored body for review",
                owner.generator.name(),
                owner.source_element_id
            ),
        ));
    }
    Ok(())
}

fn same_selected_source(
    owner: &DocumentGenerationOwner,
    current: &KnowledgeDocument,
    context: &mut PluginContext<'_>,
) -> Result<bool, PluginError> {
    let Some(previous) = current.metadata["generation"]["source_element_id"].as_str() else {
        return Ok(false);
    };
    if previous == owner.source_element_id {
        return Ok(true);
    }
    let sources = crate::source_revisions::SelectedSources::load(
        context,
        &current.project_root,
        std::collections::BTreeSet::from([previous.to_owned()]),
        None,
    )?;
    Ok(sources.matches(previous, &owner.source_element_id))
}

fn preserve_authored_context(
    proposal: &mut KnowledgeDocument,
    current: &KnowledgeDocument,
) -> Result<(), PluginError> {
    let mut metadata = current.metadata.as_object().cloned().ok_or_else(|| {
        super::invalid(
            "invalid_generated_metadata",
            &current.metadata.to_string(),
            "object metadata",
        )
    })?;
    metadata.extend(
        proposal
            .metadata
            .as_object()
            .expect("validated proposal metadata")
            .clone(),
    );
    proposal.metadata = Value::Object(metadata);
    proposal.path = current.path.clone();
    proposal.tags = current.tags.clone();
    proposal.semantic_element_id = current.semantic_element_id.clone();
    proposal.attachment_owner_id = current.attachment_owner_id.clone();
    proposal.lifecycle = current.lifecycle.clone();
    Ok(())
}

fn marker(
    owner: &DocumentGenerationOwner,
    proposal: &KnowledgeDocument,
    fields: &[String],
) -> Value {
    json!({"owner":owner.generator.name(),"source_element_id":owner.source_element_id,"metadata_fields":fields,"content_sha256":signature(proposal,fields)})
}

/// Uses the same generated-content fence for delegated memory maintenance.
/// Example: validate before applying the original, revision-checked content proposal.
pub(super) fn require_memory_hook(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
    delegation: &str,
) -> Result<(), PluginError> {
    validate_ownership(
        &DocumentGenerator::MemoryHook.owner(delegation),
        document,
        context,
    )
}

/// Refreshes only the existing workflow marker after an authorized conditional update.
/// Example: stamp the proposed content after `require_memory_hook` accepted its original body.
pub(super) fn stamp_memory_hook(document: &mut KnowledgeDocument, delegation: &str) {
    let fields = managed_fields(document);
    document.metadata["generation"] = marker(
        &DocumentGenerator::MemoryHook.owner(delegation),
        document,
        &fields,
    );
}

/// Arms an explicitly configured hook without changing authored content or taking another workflow.
/// Example: the document owner calls this only after checking the original locator and revision.
pub(super) fn arm_memory_hook(
    document: &mut KnowledgeDocument,
    delegation: &str,
) -> Result<(), PluginError> {
    if document.metadata.is_null() {
        document.metadata = json!({});
    }
    validate(&DocumentGenerator::MemoryHook.owner(delegation), document)?;
    let generation = &document.metadata["generation"];
    if !crate::lifecycle::is_active(&document.lifecycle, Utc::now())
        || (!generation.is_null()
            && (generation["owner"] != DocumentGenerator::MemoryHook.name()
                || generation["source_element_id"] != delegation))
    {
        return Err(super::invalid(
            "memory_hook_workflow_conflict",
            &document.document_id,
            "an active authored document or this hook's existing document",
        ));
    }
    stamp_memory_hook(document, delegation);
    Ok(())
}

pub(super) fn managed_fields(document: &KnowledgeDocument) -> Vec<String> {
    let mut fields = document
        .metadata
        .as_object()
        .expect("validated metadata")
        .keys()
        .filter(|key| key.as_str() != "generation" && key.as_str() != super::maintenance::MARKER)
        .cloned()
        .collect::<Vec<_>>();
    fields.sort();
    fields
}

pub(super) fn signature(document: &KnowledgeDocument, fields: &[String]) -> String {
    let metadata = fields
        .iter()
        .map(|field| (field.clone(), document.metadata[field].clone()))
        .collect::<serde_json::Map<_, _>>();
    let canonical = crate::projection_identity::canonical_projection_json(&json!([
        document.title,
        document.content,
        document.references,
        metadata
    ]));
    format!("{:x}", Sha256::digest(canonical.as_bytes()))
}

fn refuse_global_collision(
    context: &mut PluginContext<'_>,
    proposal: &KnowledgeDocument,
) -> Result<(), PluginError> {
    if crate::storage::has_knowledge_record(context, &proposal.document_id)? {
        return Err(super::invalid(
            "document_id_conflict",
            &proposal.document_id,
            "an unused document identity; review the existing Knowledge artifact before generating",
        ));
    }
    if records::find(context, &proposal.project_root, &proposal.document_id)?.is_none() {
        return Ok(());
    }
    Err(super::invalid(
        "document_exists",
        &proposal.document_id,
        "an unused document identity; automatic work cannot take over another project",
    ))
}
