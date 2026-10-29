//! Owns portable record validation shared by persistence adapters.
//! Engines call the inherent record methods; validation details stay private.
use crate::{
    SemanticArtifact, SemanticElement, SemanticPartition, SemanticRelationship,
    SemanticRevisionError, SemanticRevisionResult,
};

impl SemanticElement {
    /// Checks the same identity and required fields accepted by persistence.
    /// Example: `element.validate()?`.
    pub fn validate(&self) -> SemanticRevisionResult<()> {
        validate_element(self)
    }
}
impl SemanticArtifact {
    /// Checks the artifact shell and owner identity before publication.
    /// Example: `artifact.validate()?`.
    pub fn validate(&self) -> SemanticRevisionResult<()> {
        validate_artifact(self)
    }
    /// Checks the artifact fields before an owner has been resolved.
    /// Example: `artifact.validate_shell()?`.
    pub fn validate_shell(&self) -> SemanticRevisionResult<()> {
        validate_artifact_shell(self)
    }
}
impl SemanticRelationship {
    /// Checks required relationship fields independently of engine endpoints.
    /// Example: `relationship.validate()?`.
    pub fn validate(&self) -> SemanticRevisionResult<()> {
        validate_relationship(self)
    }
}
impl SemanticPartition {
    /// Checks the project, optional provider, and required replacement selection.
    /// Example: `partition.validate()?`.
    pub fn validate(&self) -> SemanticRevisionResult<()> {
        validate_partition(self)
    }
    /// Selects an exact path or a descendant separated by `/`.
    /// Example: `assert!(partition.contains_path("src/lib.rs"))`.
    pub fn contains_path(&self, path: &str) -> bool {
        if self.replace_paths.is_empty() {
            return self.provider_instance_id.is_some();
        }
        self.replace_paths.iter().any(|replace| {
            path == replace || path.starts_with(&format!("{}/", replace.trim_end_matches('/')))
        })
    }
}
pub(crate) fn require_non_empty(value: &str, expected: &str) -> SemanticRevisionResult<()> {
    if value.trim().is_empty() {
        return Err(SemanticRevisionError::invalid_value(value, expected));
    }
    Ok(())
}

fn validate_element(element: &SemanticElement) -> SemanticRevisionResult<()> {
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
        && crate::ContentFingerprintParts::parse(fingerprint).is_none()
    {
        return Err(SemanticRevisionError::invalid_value(
            fingerprint,
            "versioned fp1:<16-hex-simhash>:<exact-hash> content fingerprint",
        ));
    }
    validate_element_dependencies(element)
}

fn validate_element_dependencies(element: &SemanticElement) -> SemanticRevisionResult<()> {
    let mut references = std::collections::BTreeSet::new();
    for reference in &element.revision_dependencies {
        require_non_empty(
            &reference.record_id,
            "a non-empty exact revision dependency ID",
        )?;
        if !references.insert(reference) {
            return Err(SemanticRevisionError::invalid_value(
                format!("{reference:?}"),
                "one dependency per exact record kind and revision ID",
            ));
        }
    }
    Ok(())
}

fn validate_artifact(artifact: &SemanticArtifact) -> SemanticRevisionResult<()> {
    validate_artifact_shell(artifact)?;
    lumvise_contracts::validate_artifact_dependencies(
        &artifact.artifact_id,
        &artifact.dependencies,
    )
    .map_err(|error| SemanticRevisionError::invalid_value(&artifact.artifact_id, error))?;
    require_non_empty(
        &artifact.semantic_element_id,
        "non-empty semantic element id",
    )
}

fn validate_artifact_shell(artifact: &SemanticArtifact) -> SemanticRevisionResult<()> {
    require_non_empty(&artifact.artifact_id, "non-empty semantic artifact id")?;
    require_non_empty(&artifact.artifact_kind, "non-empty semantic artifact kind")?;
    require_non_empty(&artifact.title, "non-empty semantic artifact title")
}

fn validate_relationship(relationship: &SemanticRelationship) -> SemanticRevisionResult<()> {
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
fn validate_partition(partition: &SemanticPartition) -> SemanticRevisionResult<()> {
    require_non_empty(&partition.project_root, "non-empty project root")?;
    if let Some(provider) = &partition.provider_instance_id {
        require_non_empty(provider, "non-empty semantic provider identity")?;
    }
    if let Some(empty) = partition
        .replace_paths
        .iter()
        .find(|path| path.trim().is_empty())
    {
        return Err(SemanticRevisionError::invalid_value(
            empty,
            "non-empty semantic replace path",
        ));
    }
    if !partition.replace_paths.is_empty() || partition.provider_instance_id.is_some() {
        return Ok(());
    }
    Err(SemanticRevisionError::invalid_value(
        "[]",
        "at least one semantic replace path",
    ))
}
