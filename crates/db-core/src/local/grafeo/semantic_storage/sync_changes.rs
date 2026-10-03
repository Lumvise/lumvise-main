use crate::local::grafeo::graph_rows::ChangeCollector;
use crate::{Result, SemanticElement, SemanticRelationship};
use std::collections::BTreeSet;
pub(super) fn inactive_elements(
    existing: &[SemanticElement],
    element_ids: &[String],
) -> Vec<SemanticElement> {
    let inactive_ids = element_ids.iter().collect::<BTreeSet<_>>();
    existing
        .iter()
        .filter(|element| inactive_ids.contains(&element.semantic_element_id))
        .cloned()
        .map(|mut element| {
            element.lifecycle = "inactive".to_string();
            element
        })
        .collect()
}

fn existing_elements_by_id(
    elements: &[SemanticElement],
) -> std::collections::BTreeMap<String, &SemanticElement> {
    elements
        .iter()
        .map(|element| (element.semantic_element_id.clone(), element))
        .collect()
}

pub(super) fn changed_semantic_elements(
    existing: &[SemanticElement],
    active: &[SemanticElement],
    inactive: &[SemanticElement],
) -> Vec<SemanticElement> {
    let existing_by_id = existing_elements_by_id(existing);
    let duplicate_ids = duplicate_element_ids(existing);
    active
        .iter()
        .chain(inactive.iter())
        .filter(|element| {
            duplicate_ids.contains(element.semantic_element_id.as_str())
                || existing_by_id.get(&element.semantic_element_id) != Some(element)
        })
        .cloned()
        .collect()
}

pub(super) fn sort_relationships(
    mut relationships: Vec<SemanticRelationship>,
) -> Vec<SemanticRelationship> {
    relationships.sort_by(relationship_order);
    relationships
}

pub(super) fn duplicate_element_ids(elements: &[SemanticElement]) -> BTreeSet<&str> {
    let mut seen = BTreeSet::new();
    elements
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .filter(|id| !seen.insert(*id))
        .collect()
}

fn relationship_order(
    left: &SemanticRelationship,
    right: &SemanticRelationship,
) -> std::cmp::Ordering {
    left.source_element_id
        .cmp(&right.source_element_id)
        .then(left.target_element_id.cmp(&right.target_element_id))
        .then(left.relationship_kind.cmp(&right.relationship_kind))
        .then(left.label.cmp(&right.label))
}

pub(super) fn should_replace_snapshot(
    existing: &[SemanticElement],
    element_changes: &[SemanticElement],
) -> bool {
    existing.is_empty() || element_changes_are_large(existing, element_changes)
}

fn element_changes_are_large(
    existing: &[SemanticElement],
    element_changes: &[SemanticElement],
) -> bool {
    element_changes.len() > 512 && element_changes.len() * 4 > existing.len()
}

pub(super) fn record_sync_changes(
    _collector: &mut ChangeCollector,
    _project_root: &str,
    _remaps: &[crate::domain::matching::SemanticIdentityRemap],
) -> Result<()> {
    Ok(())
}
