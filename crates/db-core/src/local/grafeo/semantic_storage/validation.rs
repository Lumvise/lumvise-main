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
    require_non_empty(&element.project_root, "non-empty project root")?;
    require_non_empty(
        &element.semantic_element_id,
        "non-empty semantic element id",
    )?;
    require_non_empty(&element.semantic_source_id, "non-empty semantic source id")?;
    require_non_empty(&element.path, "non-empty semantic element path")?;
    require_non_empty(&element.element_kind, "non-empty semantic element kind")?;
    require_non_empty(&element.name, "non-empty semantic element name")?;
    if let Some(fingerprint) = element.content_fingerprint.as_deref()
        && parse_content_fingerprint(fingerprint).is_none()
    {
        return Err(DbError::invalid_value(
            fingerprint,
            "versioned fp1:<16-hex-simhash>:<exact-hash> content fingerprint",
        ));
    }
    Ok(())
}

pub(crate) fn validate_artifact(artifact: &SemanticArtifact) -> Result<()> {
    validate_artifact_shell(artifact)?;
    require_non_empty(
        &artifact.semantic_element_id,
        "non-empty semantic element id",
    )
}

pub(crate) fn validate_artifact_shell(artifact: &SemanticArtifact) -> Result<()> {
    require_non_empty(&artifact.artifact_id, "non-empty semantic artifact id")?;
    require_non_empty(&artifact.artifact_kind, "non-empty semantic artifact kind")?;
    require_non_empty(&artifact.title, "non-empty semantic artifact title")
}

pub(super) fn validate_relationship(relationship: &SemanticRelationship) -> Result<()> {
    require_non_empty(&relationship.project_root, "non-empty project root")?;
    require_non_empty(
        &relationship.source_element_id,
        "non-empty source semantic element id",
    )?;
    require_non_empty(
        &relationship.target_element_id,
        "non-empty target semantic element id",
    )?;
    require_non_empty(
        &relationship.relationship_kind,
        "non-empty relationship kind",
    )?;
    require_non_empty(&relationship.label, "non-empty relationship label")
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
    require_non_empty(&partition.project_root, "non-empty project root")?;
    if let Some(empty) = partition
        .replace_paths
        .iter()
        .find(|path| path.trim().is_empty())
    {
        return Err(DbError::invalid_value(
            empty,
            "non-empty semantic replace path",
        ));
    }
    if !partition.replace_paths.is_empty() {
        return Ok(());
    }
    Err(DbError::invalid_value(
        "[]",
        "at least one semantic replace path",
    ))
}

pub(super) fn elements_in_partition(
    elements: &[SemanticElement],
    replace_paths: &[String],
) -> Vec<SemanticElement> {
    elements
        .iter()
        .filter(|element| path_is_in_partition(&element.path, replace_paths))
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

pub(super) fn path_is_in_partition(path: &str, replace_paths: &[String]) -> bool {
    replace_paths.iter().any(|replace_path| {
        path == replace_path || path.starts_with(&partition_prefix(replace_path))
    })
}

pub(super) fn partition_prefix(path: &str) -> String {
    format!("{}/", path.trim_end_matches('/'))
}

pub(super) fn owned_partition_source_ids(
    reconciled: &crate::local::grafeo::matching::ReconciledElements,
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
