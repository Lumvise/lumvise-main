//! Owns current-owner notifications derived from accepted artifact projection changes.
//! The parent prepares these nodes and persists the same marker used by cold hook reads.
use super::*;

pub(super) fn artifact_notification_owners(
    state: &SemanticRevisionState,
    changes: &RevisionProjectionChanges<'_>,
    before: Option<&SemanticRevisionSelectionState>,
    after: Option<&SemanticRevisionSelectionState>,
    changed: &[SemanticRecordRef],
) -> BTreeSet<SemanticRecordRef> {
    let artifacts = changed
        .iter()
        .filter(|record| record.kind == SemanticRecordKind::Artifact)
        .map(|record| record.record_id.as_str())
        .collect::<BTreeSet<_>>();
    state
        .associations
        .iter()
        .chain(changes.associations.iter())
        .filter(|association| {
            artifacts.contains(association.artifact_id.as_str())
                && (head_attaches_association(before, association)
                    || head_attaches_association(after, association))
        })
        .map(|association| SemanticRecordRef::element(&association.semantic_element_id))
        .collect()
}

fn head_attaches_association(
    head: Option<&SemanticRevisionSelectionState>,
    association: &crate::SemanticRevisionAssociation,
) -> bool {
    head.is_some_and(|head| {
        head.selection.source_context == association.source_context
            && head.records.contains(&SemanticRecordRef::element(
                &association.semantic_element_id,
            ))
    })
}

pub(super) fn include_notification_nodes(
    database: &GrafeoDB,
    root: &str,
    nodes: &mut RevisionNodeIndex,
    dirty: &[(SemanticElement, ChangeDisposition)],
) -> Result<()> {
    for (element, _) in dirty {
        let record = SemanticRecordRef::element(&element.semantic_element_id);
        if nodes.contains_key(&record) {
            continue;
        }
        nodes.insert(
            record.clone(),
            existing_projection_node(database, &record, root)?,
        );
    }
    Ok(())
}
