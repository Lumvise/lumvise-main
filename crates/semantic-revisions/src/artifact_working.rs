//! Owns artifact edit tokens and irreversible freezing. Publication and selection
//! call this module; adapters persist its records without deciding draft policy.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};
mod freeze;
pub(crate) use freeze::{freeze_initial_dependencies, freeze_retained_states};

pub(crate) fn checkpoints(
    request: &SemanticRevisionPublication,
    remaps: &[SemanticRevisionRemap],
) -> Vec<SemanticRecordRef> {
    let requested = request
        .artifacts
        .iter()
        .filter(|write| write.checkpoint)
        .map(|write| SemanticRecordRef::artifact(&write.artifact.artifact_id))
        .collect::<BTreeSet<_>>();
    remaps
        .iter()
        .filter(|remap| requested.contains(&remap.incoming))
        .map(|remap| remap.accepted.clone())
        .collect()
}

pub(crate) fn edit_versions(
    selected: &[SemanticRecordRef],
    records: &[SemanticStoredRevision],
) -> BTreeMap<String, u64> {
    let selected = selected.iter().collect::<BTreeSet<_>>();
    records
        .iter()
        .filter(|row| selected.contains(&row.record))
        .filter_map(|row| {
            row.artifact_state
                .as_ref()
                .map(|state| (row.record.record_id.clone(), state.edit_version))
        })
        .collect()
}

pub(crate) fn conflicting_edits(
    state: &SemanticRevisionState,
    expected: &SemanticSelectionExpectation,
    actual: &BTreeSet<SemanticRecordRef>,
) -> BTreeSet<SemanticRecordRef> {
    state
        .records
        .iter()
        .filter(|row| actual.contains(&row.record))
        .filter_map(|row| {
            let version = row.artifact_state.as_ref()?.edit_version;
            (expected.artifact_edits.get(&row.record.record_id) != Some(&version))
                .then(|| row.record.clone())
        })
        .collect()
}

pub(crate) fn publication_dependencies(
    request: &SemanticRevisionPublication,
) -> BTreeSet<SemanticRecordRef> {
    let elements = request
        .structure
        .iter()
        .flat_map(|structure| &structure.elements)
        .flat_map(SemanticElement::required_revisions);
    let artifacts = request
        .artifacts
        .iter()
        .flat_map(|write| write.artifact.revision_dependencies());
    elements.chain(artifacts).collect()
}

pub(crate) fn can_edit(
    previous: &SemanticStoredRevision,
    artifact: &SemanticArtifact,
    required: &BTreeSet<SemanticRecordRef>,
) -> bool {
    previous
        .artifact_state
        .as_ref()
        .is_some_and(|state| !state.frozen)
        && !required.contains(&previous.record)
        && matches!(&previous.body, SemanticRevisionBody::Artifact(old)
            if old.semantic_element_id == artifact.semantic_element_id)
}

pub(crate) fn update_working(
    previous: &SemanticStoredRevision,
    artifact: SemanticArtifact,
) -> SemanticRevisionResult<SemanticStoredRevision> {
    let mut stored = previous.clone();
    let state = stored
        .artifact_state
        .as_mut()
        .expect("artifact publication checked working state");
    state.edit_version = state.edit_version.checked_add(1).ok_or_else(|| {
        SemanticRevisionError::invalid_value(
            &stored.record.record_id,
            "an advanceable artifact edit version",
        )
    })?;
    stored.body = SemanticRevisionBody::Artifact(artifact);
    Ok(stored)
}

pub(crate) fn conflicting_dependencies(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
) -> BTreeSet<SemanticRecordRef> {
    let mut versions = request.expected.artifact_edits.clone();
    let mut conflicts = BTreeSet::new();
    for dependency in request
        .artifacts
        .iter()
        .flat_map(|write| &write.artifact.dependencies)
    {
        merge_citation_version(dependency, &mut versions, &mut conflicts);
    }
    conflicts.extend(conflicting_references(
        state,
        &publication_dependencies(request),
        &versions,
    ));
    conflicts
}

fn merge_citation_version(
    dependency: &lumvise_contracts::ArtifactDependency,
    versions: &mut BTreeMap<String, u64>,
    conflicts: &mut BTreeSet<SemanticRecordRef>,
) {
    let lumvise_contracts::ArtifactDependencyTarget::Artifact {
        artifact_id,
        edit_version: Some(version),
    } = &dependency.target
    else {
        return;
    };
    if versions
        .insert(artifact_id.clone(), *version)
        .is_some_and(|old| old != *version)
    {
        conflicts.insert(SemanticRecordRef::artifact(artifact_id));
    }
}

pub(crate) fn conflicting_references(
    state: &SemanticRevisionState,
    references: &BTreeSet<SemanticRecordRef>,
    versions: &BTreeMap<String, u64>,
) -> BTreeSet<SemanticRecordRef> {
    state
        .records
        .iter()
        .filter(|row| references.contains(&row.record))
        .filter_map(|row| {
            let status = row.artifact_state.as_ref()?;
            let expected = versions.get(&row.record.record_id);
            ((!status.frozen || expected.is_some()) && expected != Some(&status.edit_version))
                .then(|| row.record.clone())
        })
        .collect()
}
