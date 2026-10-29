//! Owns qualified reference resolution. Callers use the signed reference export;
//! publication reuses `resolve` without replacing a caller's original edit token.
mod publication;
mod schema;
mod selection;
pub(super) use publication::prepare;

use lumvise_contracts::{
    KnowledgeCitationBody, KnowledgeCitationRead, KnowledgeCitationReadRequest,
    KnowledgeCitationReadState, KnowledgeCitationTarget, KnowledgeDocumentCitationSelection,
    KnowledgeProjectIdentity, KnowledgeQualifiedCitation, KnowledgeRevisionCitationSelection,
    SemanticRecordRef,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use lumvise_semantic_revisions::{SemanticRevisionBody, SemanticRevisionResolution};
use serde_json::{Value, json};

pub(super) use schema::{qualified as qualified_schema, read_input, read_output};

pub(super) fn invoke(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: KnowledgeCitationReadRequest =
        crate::parse(input, "a project-scoped citation read")?;
    let project = project_identity(context, &request.project_root)?;
    resolve(
        context,
        &KnowledgeQualifiedCitation {
            project,
            target: request.target,
        },
        false,
    )
    .map(|read| json!(read))
}

pub(super) fn project_identity(
    context: &mut PluginContext<'_>,
    root: &str,
) -> Result<KnowledgeProjectIdentity, PluginError> {
    let output = context.host_call(
        "runtime.knowledge_scope",
        json!({"operation":"citation",
        "request":{"operation":"project", "project_root":root}}),
    )?;
    crate::parse(
        output["project"].clone(),
        "the host-authorized project identity",
    )
}

pub(super) fn resolve(
    context: &mut PluginContext<'_>,
    citation: &KnowledgeQualifiedCitation,
    saving: bool,
) -> Result<KnowledgeCitationRead, PluginError> {
    require_project(context, &citation.project)?;
    crate::artifact::require(citation.target.identity().1, "citation target identity")?;
    let mut read = KnowledgeCitationRead {
        reference: citation.clone(),
        state: KnowledgeCitationReadState::Missing,
        body: None,
    };
    match &citation.target {
        KnowledgeCitationTarget::Document {
            document_id,
            selection,
        } => read_document(context, &mut read, document_id, selection)?,
        target => read_record(context, &mut read, target, saving)?,
    }
    require_project(context, &citation.project)?;
    Ok(read)
}

fn require_project(
    context: &mut PluginContext<'_>,
    expected: &KnowledgeProjectIdentity,
) -> Result<(), PluginError> {
    let actual = project_identity(context, &expected.project_root)?;
    if actual == *expected {
        return Ok(());
    }
    Err(invalid(
        &json!(expected),
        &format!("the current authorized project identity {actual:?}"),
    ))
}

fn read_record(
    context: &mut PluginContext<'_>,
    read: &mut KnowledgeCitationRead,
    target: &KnowledgeCitationTarget,
    saving: bool,
) -> Result<(), PluginError> {
    let Some((record, source_context)) =
        selection::record(context, &read.reference.project.project_root, target)?
    else {
        return Ok(());
    };
    let Some(revision) =
        crate::storage::read_revision(context, &read.reference.project.project_root, record)?
    else {
        return Ok(());
    };
    read.state = KnowledgeCitationReadState::Available;
    let exact = KnowledgeRevisionCitationSelection::Exact { source_context };
    match &revision.body {
        SemanticRevisionBody::Pruned => read.state = KnowledgeCitationReadState::Pruned,
        SemanticRevisionBody::Element(element) => {
            read.reference.target = KnowledgeCitationTarget::SemanticElement {
                semantic_element_id: element.semantic_element_id.clone(),
                selection: exact,
            };
            read.body = Some(KnowledgeCitationBody::SemanticElement {
                element: crate::parse(json!(element), "an exact semantic citation body")?,
            });
        }
        SemanticRevisionBody::Artifact(_) => read_artifact(read, target, revision, exact, saving)?,
    }
    Ok(())
}

fn read_artifact(
    read: &mut KnowledgeCitationRead,
    target: &KnowledgeCitationTarget,
    revision: SemanticRevisionResolution,
    selection: KnowledgeRevisionCitationSelection,
    saving: bool,
) -> Result<(), PluginError> {
    let KnowledgeCitationTarget::Artifact { edit_version, .. } = target else {
        return Err(invalid(
            &json!(target),
            "an artifact citation for an artifact body",
        ));
    };
    let version = revision
        .artifact_state
        .as_ref()
        .map(|state| state.edit_version);
    if edit_version.is_some() && *edit_version != version
        || saving
            && edit_version.is_none()
            && revision
                .artifact_state
                .as_ref()
                .is_some_and(|state| !state.frozen)
    {
        return Err(invalid(
            &json!(target),
            "the original observed working artifact edit version",
        ));
    }
    let SemanticRevisionBody::Artifact(artifact) = revision.body else {
        unreachable!()
    };
    let mut stored = json!(artifact);
    stored["project_root"] = json!(read.reference.project.project_root);
    stored["edit_version"] = json!(version);
    read.reference.target = KnowledgeCitationTarget::Artifact {
        artifact_id: artifact.artifact_id,
        edit_version: version,
        selection,
    };
    read.body = Some(KnowledgeCitationBody::Artifact {
        artifact: crate::storage::decode_graph_artifact(&stored)?,
    });
    Ok(())
}

fn read_document(
    context: &mut PluginContext<'_>,
    read: &mut KnowledgeCitationRead,
    document_id: &str,
    selection: &KnowledgeDocumentCitationSelection,
) -> Result<(), PluginError> {
    let root = &read.reference.project.project_root;
    let (locator, source_context) = match selection {
        KnowledgeDocumentCitationSelection::Current => (
            super::records::selected_locator(context, root, document_id)?,
            "local".to_owned(),
        ),
        KnowledgeDocumentCitationSelection::Exact {
            semantic_element_id,
            source_context,
        } => {
            crate::artifact::require(source_context, "exact document source context")?;
            let revision = crate::storage::read_revision(
                context,
                root,
                SemanticRecordRef::element(semantic_element_id),
            )?;
            (
                document_locator(revision, &mut read.state)?,
                source_context.clone(),
            )
        }
    };
    let Some(locator) = locator else {
        return Ok(());
    };
    super::spaces::require_element(context, &locator)?;
    let document = super::records::exact_document(context, locator)?;
    if document.document_id != document_id {
        return Err(invalid(
            &json!(document.document_id),
            &format!("exact cited document {document_id}"),
        ));
    }
    read.reference.target = KnowledgeCitationTarget::Document {
        document_id: document_id.into(),
        selection: KnowledgeDocumentCitationSelection::Exact {
            semantic_element_id: document.semantic_element_id.clone(),
            source_context,
        },
    };
    read.state = KnowledgeCitationReadState::Available;
    read.body = Some(KnowledgeCitationBody::Document { document });
    Ok(())
}

fn document_locator(
    revision: Option<SemanticRevisionResolution>,
    state: &mut KnowledgeCitationReadState,
) -> Result<Option<Value>, PluginError> {
    match revision.map(|revision| revision.body) {
        None => Ok(None),
        Some(SemanticRevisionBody::Pruned) => {
            *state = KnowledgeCitationReadState::Pruned;
            Ok(None)
        }
        Some(SemanticRevisionBody::Element(element)) => Ok(Some(json!(element))),
        Some(other) => Err(invalid(&json!(other), "an exact document locator")),
    }
}

fn invalid(value: &Value, expected: &str) -> PluginError {
    super::invalid(
        "knowledge_citation_unavailable",
        &value.to_string(),
        expected,
    )
}
