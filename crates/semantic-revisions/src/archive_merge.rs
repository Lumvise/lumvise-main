//! Lossless archive reconciliation preserves accepted history and rejects divergent identities.
use crate::revision_index::ensure_root;
mod reserved;
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

impl SemanticRevisionState {
    /// Restores compatible history without overwriting another accepted branch or immutable body.
    /// Example: `current.merge_archive(&archived, now_ms)?` before atomic adapter publication.
    pub fn merge_archive(&self, archived: &Self, now_ms: i64) -> SemanticRevisionResult<Self> {
        ensure_root(&archived.project_root, &self.project_root)?;
        let project_id = merge_project_identity(self, archived)?;
        reserved::validate(self, archived)?;
        let heads = merge_heads(&self.selections, &archived.selections)?;
        let records = merge_records(&self.records, &archived.records, &heads, now_ms)?;
        let mut rows = merge_history(self, archived)?;
        rows.push(SemanticRevisionMetadata::Project(project_id));
        let combined = Self {
            selections: heads,
            records,
            ..Self::default()
        };
        rows.extend(combined.metadata_rows().into_iter().filter(|row| {
            matches!(
                row,
                SemanticRevisionMetadata::Record(_)
                    | SemanticRevisionMetadata::Selection(_)
                    | SemanticRevisionMetadata::Membership(_)
            )
        }));
        let (elements, artifacts) = available_bodies(&combined.records);
        Self::from_metadata_rows(
            &self.project_root,
            self.commit_version.max(archived.commit_version),
            rows,
            elements,
            artifacts,
        )
    }
}

fn merge_project_identity(
    current: &SemanticRevisionState,
    archived: &SemanticRevisionState,
) -> SemanticRevisionResult<String> {
    crate::revision_index::require_project_identity(&archived.project_id)?;
    let has_history = !current.records.is_empty()
        || !current.selection_history.is_empty()
        || !current.accepted_operations.is_empty()
        || !current.retention_operations.is_empty();
    if has_history && current.project_id != archived.project_id {
        return Err(archive_conflict(
            &archived.project_id,
            format!("the existing project identity {}", current.project_id),
        ));
    }
    Ok(archived.project_id.clone())
}

fn merge_heads(
    current: &[SemanticRevisionSelectionState],
    archived: &[SemanticRevisionSelectionState],
) -> SemanticRevisionResult<Vec<SemanticRevisionSelectionState>> {
    let mut heads = BTreeMap::<&str, &SemanticRevisionSelectionState>::new();
    for head in current.iter().chain(archived) {
        let id = head.selection.selection_id.as_str();
        if let Some(previous) = heads.get(id) {
            if previous.selection.generation == head.selection.generation && *previous != head {
                return Err(archive_conflict(
                    id,
                    "the same selection at an acknowledged generation",
                ));
            }
            if previous.selection.generation >= head.selection.generation {
                continue;
            }
        }
        heads.insert(id, head);
    }
    Ok(heads.into_values().cloned().collect())
}

fn merge_records(
    current: &[SemanticStoredRevision],
    archived: &[SemanticStoredRevision],
    heads: &[SemanticRevisionSelectionState],
    now_ms: i64,
) -> SemanticRevisionResult<Vec<SemanticStoredRevision>> {
    let selected = heads
        .iter()
        .flat_map(|head| head.records.iter())
        .collect::<BTreeSet<_>>();
    let mut records = BTreeMap::<SemanticRecordRef, SemanticStoredRevision>::new();
    for stored in current.iter().chain(archived) {
        match records.get_mut(&stored.record) {
            Some(previous) => merge_record(previous, stored, now_ms)?,
            None => {
                records.insert(stored.record.clone(), stored.clone());
            }
        }
    }
    for stored in records.values_mut() {
        if selected.contains(&stored.record) {
            stored.unselected_at_ms = None;
        } else if stored.unselected_at_ms.is_none() {
            stored.unselected_at_ms = Some(now_ms);
        }
    }
    Ok(records.into_values().collect())
}

fn merge_record(
    previous: &mut SemanticStoredRevision,
    incoming: &SemanticStoredRevision,
    now_ms: i64,
) -> SemanticRevisionResult<()> {
    merge_artifact_state(previous, incoming)?;
    if previous.element_scope != incoming.element_scope {
        return Err(archive_conflict(
            &incoming.record.record_id,
            "identical exact revision ownership",
        ));
    }
    if matches!(previous.body, SemanticRevisionBody::Pruned)
        && !matches!(incoming.body, SemanticRevisionBody::Pruned)
    {
        previous.body = incoming.body.clone();
        previous.pruned_at_ms = None;
        previous.unselected_at_ms = Some(now_ms);
        return Ok(());
    }
    if !matches!(incoming.body, SemanticRevisionBody::Pruned) && previous.body != incoming.body {
        return Err(archive_conflict(
            &incoming.record.record_id,
            "identical immutable revision content",
        ));
    }
    previous.unselected_at_ms = match (previous.unselected_at_ms, incoming.unselected_at_ms) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    };
    Ok(())
}

fn merge_artifact_state(
    previous: &mut SemanticStoredRevision,
    incoming: &SemanticStoredRevision,
) -> SemanticRevisionResult<()> {
    match (&mut previous.artifact_state, &incoming.artifact_state) {
        (Some(stored), Some(archived)) if stored.edit_version == archived.edit_version => {
            stored.frozen |= archived.frozen;
            Ok(())
        }
        (None, None) => Ok(()),
        _ => Err(archive_conflict(
            &incoming.record.record_id,
            "the same artifact edit version; working bodies cannot be silently replaced by an archive",
        )),
    }
}

fn merge_history(
    current: &SemanticRevisionState,
    archived: &SemanticRevisionState,
) -> SemanticRevisionResult<Vec<SemanticRevisionMetadata>> {
    let mut rows = BTreeMap::<(&str, String), SemanticRevisionMetadata>::new();
    for row in current
        .metadata_rows()
        .into_iter()
        .chain(archived.metadata_rows())
    {
        if matches!(
            row,
            SemanticRevisionMetadata::Project(_)
                | SemanticRevisionMetadata::Record(_)
                | SemanticRevisionMetadata::Selection(_)
                | SemanticRevisionMetadata::Membership(_)
        ) {
            continue;
        }
        let key = (row.kind(), row.identity()?);
        if let Some(previous) = rows.get(&key) {
            if serde_json::to_vec(previous)? != serde_json::to_vec(&row)? {
                return Err(archive_conflict(
                    &key.1,
                    format!("identical accepted {} history", key.0),
                ));
            }
        } else {
            rows.insert(key, row);
        }
    }
    Ok(rows.into_values().collect())
}

fn available_bodies(
    records: &[SemanticStoredRevision],
) -> (Vec<SemanticElement>, Vec<SemanticArtifact>) {
    let (mut elements, mut artifacts) = (vec![], vec![]);
    for stored in records {
        match &stored.body {
            SemanticRevisionBody::Element(element) => elements.push(element.clone()),
            SemanticRevisionBody::Artifact(artifact) => artifacts.push(artifact.clone()),
            SemanticRevisionBody::Pruned => {}
        }
    }
    (elements, artifacts)
}

fn archive_conflict(value: impl Into<String>, expected: impl AsRef<str>) -> SemanticRevisionError {
    SemanticRevisionError::invalid_value(
        value,
        format!(
            "{}; restore into a separate project or resolve the divergent selection explicitly",
            expected.as_ref()
        ),
    )
}
