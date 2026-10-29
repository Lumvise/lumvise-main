//! Exact record and lineage lookup shared by publication and collection.
use crate::validation::require_non_empty;
use crate::*;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn digest(value: &impl Serialize) -> SemanticRevisionResult<String> {
    let mut writer = RevisionDigestWriter(Sha256::new());
    serde_json::to_writer(&mut writer, value)?;
    Ok(format!("{:x}", writer.0.finalize()))
}

struct RevisionDigestWriter(Sha256);
impl std::io::Write for RevisionDigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Plans an ID when a change needs a frozen-state successor or explicit checkpoint, not a working edit.
/// Publication still decides acceptance and reuses unchanged bodies; this function does not mutate state.
/// Example: `semantic_revision_successor_id("lineage", "local", "edit-1", &SemanticRecordRef::artifact("body"))?`.
pub fn revision_id(
    project_id: &str,
    source_context: &str,
    operation_id: &str,
    previous: &SemanticRecordRef,
) -> SemanticRevisionResult<String> {
    require_non_empty(project_id, "a non-empty durable project identity")?;
    require_non_empty(source_context, "a non-empty source context")?;
    require_non_empty(operation_id, "a non-empty immutable operation ID")?;
    require_non_empty(&previous.record_id, "a non-empty exact previous record ID")?;
    Ok(format!(
        "revision:{}",
        digest(&(project_id, source_context, operation_id, previous))?
    ))
}

pub(crate) fn require_project_identity(project_id: &str) -> SemanticRevisionResult<()> {
    if project_id.trim().is_empty() {
        return Err(SemanticRevisionError::invalid_value(
            project_id,
            "a non-empty durable project identity",
        ));
    }
    Ok(())
}

pub(crate) fn record_index(
    state: &SemanticRevisionState,
) -> BTreeMap<SemanticRecordRef, &SemanticStoredRevision> {
    state
        .records
        .iter()
        .map(|stored| (stored.record.clone(), stored))
        .collect()
}

pub(crate) fn selected_elements(
    state: &SemanticRevisionState,
    selected: &[SemanticRecordRef],
) -> Vec<SemanticElement> {
    let selected = selected.iter().collect::<BTreeSet<_>>();
    state
        .records
        .iter()
        .filter(|stored| selected.contains(&stored.record))
        .filter_map(|stored| match &stored.body {
            SemanticRevisionBody::Element(element) => Some(element.clone()),
            _ => None,
        })
        .collect()
}

pub(crate) fn selected_successor(
    successors: &BTreeMap<&SemanticRecordRef, Vec<&SemanticRecordRef>>,
    requested: &SemanticRecordRef,
    selected: &BTreeSet<SemanticRecordRef>,
) -> SemanticRevisionResult<Option<SemanticRecordRef>> {
    let mut pending = vec![requested.clone()];
    let mut visited = BTreeSet::new();
    let mut candidates = BTreeSet::new();
    while let Some(record) = pending.pop() {
        if !visited.insert(record.clone()) {
            continue;
        }
        if selected.contains(&record) {
            candidates.insert(record);
            continue;
        }
        pending.extend(
            successors
                .get(&record)
                .into_iter()
                .flatten()
                .map(|record| (*record).clone()),
        );
    }
    if candidates.len() > 1 {
        return Err(SemanticRevisionError::invalid_value(
            &requested.record_id,
            "one accepted successor in this selection; choose an explicit revision for a fork",
        ));
    }
    Ok(candidates.into_iter().next())
}

pub(crate) fn require_body<'a>(
    index: &'a BTreeMap<SemanticRecordRef, &'a SemanticStoredRevision>,
    record: &SemanticRecordRef,
) -> SemanticRevisionResult<&'a SemanticRevisionBody> {
    match index.get(record).map(|stored| &stored.body) {
        Some(SemanticRevisionBody::Pruned) | None => Err(SemanticRevisionError::invalid_value(
            &record.record_id,
            "an available exact revision body in this project",
        )),
        Some(body) => Ok(body),
    }
}

pub(crate) fn project_identity_conflict(
    state: &SemanticRevisionState,
    observed: &str,
    selection_id: &str,
    expected_generation: i64,
) -> Option<SemanticRevisionConflict> {
    (observed != state.project_id).then(|| SemanticRevisionConflict {
        project_root: state.project_root.clone(),
        expected_project_id: observed.into(),
        actual_project_id: state.project_id.clone(),
        selection_id: selection_id.into(),
        expected_generation,
        actual_generation: state
            .selection(selection_id)
            .map_or(0, |head| head.selection.generation),
        records: vec![],
        connection_ids: vec![],
    })
}

pub(crate) fn empty_plan(
    state: &SemanticRevisionState,
    outcome: SemanticRevisionPublicationOutcome,
) -> SemanticRevisionPublicationPlan {
    SemanticRevisionPublicationPlan {
        expected_commit_version: state.commit_version,
        outcome,
        records: vec![],
        connections: vec![],
        selections: vec![],
        selection_deltas: vec![],
        revision_links: vec![],
        associations: vec![],
        accepted_operation: None,
    }
}

pub(crate) fn unique_nonempty(values: &[String], expected: &str) -> SemanticRevisionResult<()> {
    let mut seen = BTreeSet::new();
    for value in values {
        if value.trim().is_empty() || !seen.insert(value) {
            return Err(SemanticRevisionError::invalid_value(value, expected));
        }
    }
    Ok(())
}

pub(crate) fn ensure_root(actual: &str, expected: &str) -> SemanticRevisionResult<()> {
    if actual == expected && !actual.trim().is_empty() {
        return Ok(());
    }
    Err(SemanticRevisionError::invalid_value(
        actual,
        format!("project root `{expected}`"),
    ))
}

pub(crate) fn stored_body(
    record: SemanticRecordRef,
    body: SemanticRevisionBody,
    active: bool,
    now_ms: i64,
) -> SemanticStoredRevision {
    let element_scope = match &body {
        SemanticRevisionBody::Element(element) => Some(SemanticRevisionElementScope {
            path: element.path.clone(),
            provider_instance_id: element.metadata["provider_instance_id"]
                .as_str()
                .map(str::to_owned),
        }),
        _ => None,
    };
    SemanticStoredRevision {
        artifact_state: (record.kind == SemanticRecordKind::Artifact).then_some(
            SemanticArtifactRevisionState {
                edit_version: 1,
                frozen: !active,
            },
        ),
        record,
        element_scope,
        body,
        created_at_ms: now_ms,
        unselected_at_ms: (!active).then_some(now_ms),
        pruned_at_ms: None,
    }
}

pub(crate) fn source_successors<'a>(
    state: &'a SemanticRevisionState,
    source_context: &str,
) -> BTreeMap<&'a SemanticRecordRef, Vec<&'a SemanticRecordRef>> {
    link_successors(&state.revision_links, source_context)
}

pub(crate) fn link_successors<'a>(
    links: &'a [SemanticRevisionLink],
    source_context: &str,
) -> BTreeMap<&'a SemanticRecordRef, Vec<&'a SemanticRecordRef>> {
    let mut successors = BTreeMap::<_, Vec<_>>::new();
    for link in links
        .iter()
        .filter(|link| link.source_context == source_context)
    {
        successors.entry(&link.older).or_default().push(&link.newer);
    }
    successors
}
