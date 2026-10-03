//! Owns portable record validation shared by persistence adapters.
//! Engines call the inherent record methods; validation details stay private.
use crate::{
    ContentFingerprintParts, DbError, Result, SemanticArtifact, SemanticElement, SemanticPartition,
    SemanticRelationship,
};

impl SemanticElement {
    /// Checks the same identity and required fields accepted by persistence.
    /// Example: `element.validate()?`.
    pub fn validate(&self) -> Result<()> {
        validate_element(self)
    }
}
impl SemanticArtifact {
    /// Checks the artifact shell and owner identity before publication.
    /// Example: `artifact.validate()?`.
    pub fn validate(&self) -> Result<()> {
        validate_artifact(self)
    }
    /// Checks the artifact fields before an owner has been resolved.
    /// Example: `artifact.validate_shell()?`.
    pub fn validate_shell(&self) -> Result<()> {
        validate_artifact_shell(self)
    }
}
impl SemanticRelationship {
    /// Checks required relationship fields independently of engine endpoints.
    /// Example: `relationship.validate()?`.
    pub fn validate(&self) -> Result<()> {
        validate_relationship(self)
    }
}
impl SemanticPartition {
    /// Checks the project and nonempty replacement paths.
    /// Example: `partition.validate()?`.
    pub fn validate(&self) -> Result<()> {
        validate_partition(self)
    }
    /// Selects an exact path or a descendant separated by `/`.
    /// Example: `assert!(partition.contains_path("src/lib.rs"))`.
    pub fn contains_path(&self, path: &str) -> bool {
        self.replace_paths.iter().any(|replace| {
            path == replace || path.starts_with(&format!("{}/", replace.trim_end_matches('/')))
        })
    }
}
fn require_non_empty(value: &str, expected: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(DbError::invalid_value(value, expected));
    }
    Ok(())
}

fn validate_element(element: &SemanticElement) -> Result<()> {
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
        && ContentFingerprintParts::parse(fingerprint).is_none()
    {
        return Err(DbError::invalid_value(
            fingerprint,
            "versioned fp1:<16-hex-simhash>:<exact-hash> content fingerprint",
        ));
    }
    Ok(())
}

fn validate_artifact(artifact: &SemanticArtifact) -> Result<()> {
    validate_artifact_shell(artifact)?;
    require_non_empty(
        &artifact.semantic_element_id,
        "non-empty semantic element id",
    )
}

fn validate_artifact_shell(artifact: &SemanticArtifact) -> Result<()> {
    require_non_empty(&artifact.artifact_id, "non-empty semantic artifact id")?;
    require_non_empty(&artifact.artifact_kind, "non-empty semantic artifact kind")?;
    require_non_empty(&artifact.title, "non-empty semantic artifact title")
}

fn validate_relationship(relationship: &SemanticRelationship) -> Result<()> {
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
fn validate_partition(partition: &SemanticPartition) -> Result<()> {
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
