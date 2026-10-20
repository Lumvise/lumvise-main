//! Automatic content uses the same document records and revisions as authored edits.
//! Source identity and the last generated signature prevent taking over another body.

use chrono::Utc;
use lumvise_contracts::{KnowledgeDocument, KnowledgeDocumentIdentity, KnowledgeDocumentKind};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::records::{self, DocumentState};

#[derive(Clone, Copy, Debug)]
pub(crate) enum DocumentGenerator {
    FunctionalContext,
    C4Report,
    MarkdownSeed,
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
        }
    }

    fn kind(self) -> KnowledgeDocumentKind {
        match self {
            Self::C4Report => KnowledgeDocumentKind::Report,
            _ => KnowledgeDocumentKind::Context,
        }
    }
}

pub(crate) struct DocumentGenerationOwner {
    generator: DocumentGenerator,
    source_element_id: String,
}

/// These conflicts require review rather than another automatic overwrite attempt.
/// Example: is_generation_hold(error) moves a completed job into its existing failure state.
pub(crate) fn is_generation_hold(error: &PluginError) -> bool {
    matches!(
        error.code.as_str(),
        "generated_document_authored"
            | "generated_document_deleted"
            | "generated_document_inactive"
            | "knowledge_conversion_required"
            | "document_exists"
    )
}

/// Publish one generated proposal through the canonical owner, preserving authored changes.
/// Example: save(context, &DocumentGenerator::C4Report.owner(source_id), proposal, Some(read_revision)).
pub(crate) fn save(
    context: &mut PluginContext<'_>,
    owner: &DocumentGenerationOwner,
    proposal: KnowledgeDocument,
    expected_revision: Option<u64>,
) -> Result<KnowledgeDocument, PluginError> {
    validate(owner, &proposal)?;
    let identity = KnowledgeDocumentIdentity {
        project_root: proposal.project_root.clone(),
        document_id: proposal.document_id.clone(),
    };
    let current = current(context, &identity)?;
    let mut proposal = prepare(owner, proposal, current.as_ref(), expected_revision)?;
    if current.as_ref() == Some(&proposal) {
        return Ok(proposal);
    }
    publish(context, owner, &mut proposal, current.as_ref())?;
    Ok(proposal)
}

fn prepare(
    owner: &DocumentGenerationOwner,
    mut proposal: KnowledgeDocument,
    current: Option<&KnowledgeDocument>,
    expected: Option<u64>,
) -> Result<KnowledgeDocument, PluginError> {
    let metadata_fields = managed_fields(&proposal);
    validate_expected(&proposal, current, expected)?;
    if let Some(current) = current {
        validate_ownership(owner, current)?;
        preserve_authored_context(&mut proposal, current)?;
    }
    proposal.metadata["generation"] = marker(owner, &proposal, &metadata_fields);
    Ok(proposal)
}

fn publish(
    context: &mut PluginContext<'_>,
    owner: &DocumentGenerationOwner,
    proposal: &mut KnowledgeDocument,
    current: Option<&KnowledgeDocument>,
) -> Result<(), PluginError> {
    crate::lifecycle::stamp(
        &mut proposal.lifecycle,
        current.map(|document| &document.lifecycle),
        Utc::now(),
    );
    if current.is_some() {
        return records::write(context, proposal);
    }
    refuse_global_collision(context, owner, proposal)?;
    records::create(context, proposal)
}

fn current(
    context: &mut PluginContext<'_>,
    identity: &KnowledgeDocumentIdentity,
) -> Result<Option<KnowledgeDocument>, PluginError> {
    match records::scoped_state(context, identity)? {
        DocumentState::Stored(document) => Ok(Some(document)),
        DocumentState::Missing => Ok(None),
        DocumentState::Deleted => Err(super::invalid(
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
    if proposal.document_kind != owner.generator.kind() || !proposal.metadata.is_object() {
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
    expected: Option<u64>,
) -> Result<(), PluginError> {
    if current.map(|document| document.lifecycle.revision) == expected {
        return Ok(());
    }
    Err(super::invalid(
        "document_revision_conflict",
        &format!("{} at expected revision {expected:?}", proposal.document_id),
        &format!(
            "the read revision {:?}",
            current.map(|document| document.lifecycle.revision)
        ),
    ))
}

fn validate_ownership(
    owner: &DocumentGenerationOwner,
    current: &KnowledgeDocument,
) -> Result<(), PluginError> {
    let stored = &current.metadata["generation"];
    if current.document_kind != owner.generator.kind()
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
        || stored["source_element_id"] != owner.source_element_id
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

fn managed_fields(document: &KnowledgeDocument) -> Vec<String> {
    let mut fields = document
        .metadata
        .as_object()
        .expect("validated metadata")
        .keys()
        .filter(|key| key.as_str() != "generation")
        .cloned()
        .collect::<Vec<_>>();
    fields.sort();
    fields
}

fn signature(document: &KnowledgeDocument, fields: &[String]) -> String {
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
    owner: &DocumentGenerationOwner,
    proposal: &KnowledgeDocument,
) -> Result<(), PluginError> {
    if crate::storage::has_knowledge_record(context, &proposal.document_id)? {
        return Err(super::invalid(
            "knowledge_conversion_required",
            &proposal.document_id,
            "no Knowledge artifact at this generated document identity; preview legacy conversion or review the collision before generating",
        ));
    }
    let tag = match owner.generator {
        DocumentGenerator::FunctionalContext => Some("cultivation-functional"),
        DocumentGenerator::C4Report => Some("scoped-c4"),
        DocumentGenerator::MarkdownSeed => None,
    };
    if let Some(tag) = tag
        && super::conversion::required_for_source(
            context,
            &proposal.project_root,
            &owner.source_element_id,
            tag,
        )?
    {
        return Err(super::invalid(
            "knowledge_conversion_required",
            &owner.source_element_id,
            "explicit document conversion of the retained source workflow before creating another body",
        ));
    }
    if records::find(context, &proposal.document_id)?.is_none() {
        return Ok(());
    }
    Err(super::invalid(
        "document_exists",
        &proposal.document_id,
        "an unused document identity; automatic work cannot take over another project",
    ))
}
