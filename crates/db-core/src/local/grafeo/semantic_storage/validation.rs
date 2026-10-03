use super::*;

pub(super) fn validate_unique_element_ids(elements: &[SemanticElement]) -> Result<()> {
    let mut seen = HashSet::new();
    for element in elements {
        if !seen.insert(element.semantic_element_id.as_str()) {
            return Err(DbError::invalid_value(
                &element.semantic_element_id,
                "one incoming record per semantic element id",
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_duplicate_repair_inputs(
    existing: &[SemanticElement],
    incoming: &[SemanticElement],
) -> Result<()> {
    let incoming_ids = incoming
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect::<HashSet<_>>();
    for duplicate_id in duplicate_element_ids(existing) {
        if !incoming_ids.contains(duplicate_id) {
            return Err(DbError::invalid_value(
                duplicate_id,
                "an authoritative incoming record for each duplicated stored identity",
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_element(element: &SemanticElement) -> Result<()> {
    element.validate()
}
pub(crate) fn validate_artifact(artifact: &SemanticArtifact) -> Result<()> {
    artifact.validate()
}
#[cfg(test)]
pub(crate) fn validate_artifact_shell(artifact: &SemanticArtifact) -> Result<()> {
    artifact.validate_shell()
}
pub(super) fn validate_relationship(relationship: &SemanticRelationship) -> Result<()> {
    relationship.validate()
}
pub(super) fn validate_partition_write_inputs(
    element_changes: &[SemanticElement],
    relationships: &[SemanticRelationship],
) -> Result<()> {
    for element in element_changes {
        validate_element(element)?;
    }
    for relationship in relationships {
        validate_relationship(relationship)?;
    }
    Ok(())
}

pub(super) fn validate_semantic_sync_inputs(
    active_elements: &[SemanticElement],
    inactive_elements: &[SemanticElement],
    relationships: &[SemanticRelationship],
) -> Result<()> {
    for element in active_elements.iter().chain(inactive_elements.iter()) {
        validate_element(element)?;
    }
    for relationship in relationships {
        validate_relationship(relationship)?;
    }
    Ok(())
}

pub(super) fn validate_partition(partition: &SemanticPartition) -> Result<()> {
    partition.validate()
}

pub(super) fn elements_in_partition(
    elements: &[SemanticElement],
    replace_paths: &[String],
) -> Vec<SemanticElement> {
    let partition = SemanticPartition {
        project_root: String::new(),
        replace_paths: replace_paths.to_vec(),
    };
    elements
        .iter()
        .filter(|element| partition.contains_path(&element.path))
        .cloned()
        .collect()
}

pub(super) fn partition_paths_are_exact(
    replace_paths: &[String],
    incoming_elements: &[SemanticElement],
) -> bool {
    let replace_paths = replace_paths.iter().collect::<BTreeSet<_>>();
    incoming_elements
        .iter()
        .all(|element| replace_paths.contains(&element.path))
}

pub(super) fn incoming_partition_paths(
    replace_paths: &[String],
    incoming_elements: &[SemanticElement],
) -> Vec<String> {
    replace_paths
        .iter()
        .chain(incoming_elements.iter().map(|element| &element.path))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(super) fn owned_partition_source_ids(
    reconciled: &crate::domain::matching::SemanticStructureReconciliation,
    inactive_elements: &[SemanticElement],
) -> BTreeSet<String> {
    reconciled
        .active_elements
        .iter()
        .chain(inactive_elements.iter())
        .map(|element| element.semantic_element_id.clone())
        .collect()
}

pub(super) fn relationships_owned_by_sources(
    relationships: &[SemanticRelationship],
    source_ids: &BTreeSet<String>,
) -> Vec<SemanticRelationship> {
    relationships
        .iter()
        .filter(|relationship| source_ids.contains(&relationship.source_element_id))
        .cloned()
        .collect()
}
