//! Owns relationship normalization for authoritative structure sync.
//! Engines share containment derivation, endpoint remaps, and deduplication.
use crate::domain::matching::IdentityRemapIndex;
use crate::{SemanticElement, SemanticRelationship, SemanticStructureReconciliation};
use std::collections::BTreeMap;

impl SemanticStructureReconciliation {
    /// Normalizes endpoints and derived containment using resolved identities.
    /// Example: `reconciled.normalized_relationships("/repo", &incoming)`.
    pub fn normalized_relationships(
        &self,
        project_root: &str,
        incoming: &[SemanticRelationship],
    ) -> Vec<SemanticRelationship> {
        normalized_relationships(project_root, &self.active_elements, incoming, &self.remaps)
    }
}

pub(crate) fn remap_relationships(
    relationships: &[SemanticRelationship],
    remaps: &IdentityRemapIndex<'_>,
) -> Vec<SemanticRelationship> {
    relationships
        .iter()
        .cloned()
        .map(|mut relationship| {
            relationship.source_element_id = remaps.resolve(&relationship.source_element_id);
            relationship.target_element_id = remaps.resolve(&relationship.target_element_id);
            relationship
        })
        .collect()
}

pub(crate) fn normalized_relationships(
    project_root: &str,
    elements: &[SemanticElement],
    relationships: &[SemanticRelationship],
    remaps: &[crate::domain::matching::SemanticIdentityRemap],
) -> Vec<SemanticRelationship> {
    let remap_index = IdentityRemapIndex::new(remaps);
    let mut normalized = BTreeMap::new();
    for relationship in remap_relationships(relationships, &remap_index) {
        normalized.insert(relationship_key(&relationship), relationship);
    }
    for relationship in
        contains_relationships_from_parent_hints(project_root, elements, &remap_index)
    {
        normalized
            .entry(relationship_key(&relationship))
            .or_insert(relationship);
    }
    normalized.into_values().collect()
}

pub(crate) fn contains_relationships_from_parent_hints(
    project_root: &str,
    elements: &[SemanticElement],
    remaps: &IdentityRemapIndex<'_>,
) -> Vec<SemanticRelationship> {
    elements
        .iter()
        .filter_map(|element| contains_relationship_from_parent_hint(project_root, element, remaps))
        .collect()
}

pub(crate) fn contains_relationship_from_parent_hint(
    project_root: &str,
    element: &SemanticElement,
    remaps: &IdentityRemapIndex<'_>,
) -> Option<SemanticRelationship> {
    let parent_id = element.parent_element_id.as_deref()?;
    Some(SemanticRelationship {
        project_root: project_root.to_string(),
        source_element_id: remaps.resolve(parent_id),
        target_element_id: remaps.resolve(&element.semantic_element_id),
        relationship_kind: "contains".to_string(),
        label: "contains".to_string(),
        metadata: serde_json::json!({ "derived_from": "parent_element_id" }),
    })
}

pub(crate) fn relationship_key(
    relationship: &SemanticRelationship,
) -> (String, String, String, String) {
    (
        relationship.source_element_id.clone(),
        relationship.target_element_id.clone(),
        relationship.relationship_kind.clone(),
        relationship.label.clone(),
    )
}
