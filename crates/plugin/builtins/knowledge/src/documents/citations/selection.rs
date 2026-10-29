//! Delegates family selection to shared policy; exact references never follow successors.
use super::*;
use lumvise_semantic_revisions::{SemanticRevisionRemap, SemanticRevisionSelectionObservation};

pub(super) fn record(
    context: &mut PluginContext<'_>,
    root: &str,
    target: &KnowledgeCitationTarget,
) -> Result<Option<(SemanticRecordRef, String)>, PluginError> {
    let (record, selection) = match target {
        KnowledgeCitationTarget::SemanticElement {
            semantic_element_id,
            selection,
        } => (SemanticRecordRef::element(semantic_element_id), selection),
        KnowledgeCitationTarget::Artifact {
            artifact_id,
            selection,
            ..
        } => (SemanticRecordRef::artifact(artifact_id), selection),
        _ => return Err(invalid(&json!(target), "an element or artifact citation")),
    };
    match selection {
        KnowledgeRevisionCitationSelection::Exact { source_context } => {
            crate::artifact::require(source_context, "exact source context")?;
            Ok(Some((record, source_context.clone())))
        }
        KnowledgeRevisionCitationSelection::Selected {
            selection_id,
            source_context,
        } => selected(context, root, record, selection_id, source_context),
    }
}

fn selected(
    context: &mut PluginContext<'_>,
    root: &str,
    record: SemanticRecordRef,
    selection_id: &str,
    source_context: &str,
) -> Result<Option<(SemanticRecordRef, String)>, PluginError> {
    let before = observation(context, root, selection_id)?;
    let Some(before) = before else {
        return Ok(None);
    };
    if before.selection.source_context != source_context {
        return Err(invalid(
            &json!(source_context),
            "the original selected source context",
        ));
    }
    let output = context.host_call(
        "storage.semantic",
        json!({"operation":"revision_families",
        "query":{"project_root":root, "selection_id":selection_id, "records":[record]}}),
    )?;
    let remaps: Vec<SemanticRevisionRemap> =
        crate::parse(output["remaps"].clone(), "selected family identities")?;
    if observation(context, root, selection_id)?.as_ref() != Some(&before) {
        return Err(invalid(
            &json!(selection_id),
            "unchanged membership during a citation read",
        ));
    }
    Ok(remaps
        .into_iter()
        .find(|remap| remap.incoming == record)
        .map(|remap| (remap.accepted, source_context.into())))
}

fn observation(
    context: &mut PluginContext<'_>,
    root: &str,
    selection: &str,
) -> Result<Option<SemanticRevisionSelectionObservation>, PluginError> {
    let output = context.host_call("storage.semantic", json!({"operation":"revision_selection_members",
        "query":{"project_root":root, "selection_id":selection, "structure_coverage":{"kind":"none"}, "artifact_ids":[]}}))?;
    crate::parse(
        output["membership"].clone(),
        "the selected source context and generation",
    )
}
