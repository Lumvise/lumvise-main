//! Media associations share one selector; sources supply current project records.
use crate::domain::fingerprint::fingerprint_algorithm;
use crate::{
    ContentFingerprintParts, DbError, Result, SemanticArtifact, SemanticElement,
    SemanticProjectSnapshot,
};

pub(crate) trait MediaAssociationSource {
    fn element(&self, id: &str) -> Option<SemanticElement>;
    fn candidates(&self, root: &str) -> Vec<SemanticElement>;
    fn artifacts(&self, id: &str) -> Vec<SemanticArtifact>;
}
pub(crate) fn artifact_inheritance_from_source(
    source: &dyn MediaAssociationSource,
    id: &str,
) -> Result<Vec<SemanticArtifact>> {
    if id.trim().is_empty() {
        return Err(DbError::invalid_value(id, "nonempty semantic element ID"));
    }
    let Some(target) = source.element(id) else {
        return Ok(Vec::new());
    };
    let direct = source.artifacts(id);
    if !direct.is_empty() || !is_media(&target) {
        return Ok(direct);
    }
    let candidates = source
        .candidates(&target.project_root)
        .into_iter()
        .filter(|candidate| {
            candidate.semantic_element_id != target.semantic_element_id
                && compatible(candidate, &target)
        })
        .filter(|candidate| !source.artifacts(&candidate.semantic_element_id).is_empty())
        .filter_map(|candidate| confidence(&candidate, &target).map(|exact| (candidate, exact)))
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return Ok(Vec::new());
    }
    let (candidate, exact) = &candidates[0];
    source
        .artifacts(&candidate.semantic_element_id)
        .into_iter()
        .map(|artifact| inherited(artifact, &target, candidate, *exact))
        .collect()
}
fn compatible(source: &SemanticElement, target: &SemanticElement) -> bool {
    source.project_root == target.project_root
        && source.element_kind == target.element_kind
        && source.lifecycle == "active"
        && media_kind(source) == media_kind(target)
        && fingerprint_algorithm(source) == fingerprint_algorithm(target)
}
pub(crate) fn media_kind(element: &SemanticElement) -> Option<&str> {
    (element.element_kind == "file")
        .then(|| {
            element
                .metadata
                .get("media_kind")
                .and_then(serde_json::Value::as_str)
        })
        .flatten()
        .filter(|kind| matches!(*kind, "image" | "audio" | "video"))
}
fn is_media(element: &SemanticElement) -> bool {
    media_kind(element).is_some()
        || matches!(
            element.element_kind.as_str(),
            "image" | "audio" | "video" | "image_region" | "audio_segment" | "video_segment"
        )
}
fn confidence(source: &SemanticElement, target: &SemanticElement) -> Option<bool> {
    let source = ContentFingerprintParts::parse(source.content_fingerprint.as_deref()?)?;
    let target = ContentFingerprintParts::parse(target.content_fingerprint.as_deref()?)?;
    if source.matches_exact(&target) {
        return Some(true);
    }
    (source.hamming_distance(&target)? <= 12).then_some(false)
}
fn inherited(
    mut artifact: SemanticArtifact,
    target: &SemanticElement,
    source: &SemanticElement,
    exact: bool,
) -> Result<SemanticArtifact> {
    if !artifact.metadata.is_object() {
        return Err(DbError::invalid_value(
            artifact.metadata.to_string(),
            "artifact metadata object for inherited association",
        ));
    }
    artifact.semantic_element_id = target.semantic_element_id.clone();
    artifact.metadata["association_kind"] = "inherited".into();
    artifact.metadata["association_confidence"] = if exact { 100 } else { 90 }.into();
    artifact.metadata["association_precaution"] = (!exact).into();
    artifact.metadata["inherited_from_semantic_element_id"] =
        source.semantic_element_id.clone().into();
    Ok(artifact)
}
impl MediaAssociationSource for SemanticProjectSnapshot {
    fn element(&self, id: &str) -> Option<SemanticElement> {
        self.elements
            .iter()
            .find(|element| element.semantic_element_id == id && element.lifecycle != "inactive")
            .cloned()
    }
    fn candidates(&self, root: &str) -> Vec<SemanticElement> {
        self.elements
            .iter()
            .filter(|element| element.project_root == root)
            .cloned()
            .collect()
    }
    fn artifacts(&self, id: &str) -> Vec<SemanticArtifact> {
        let mut artifacts = self
            .artifacts
            .iter()
            .filter(|artifact| {
                artifact.semantic_element_id == id
                    && artifact.metadata["association_kind"] != "inherited"
            })
            .cloned()
            .collect::<Vec<_>>();
        artifacts.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
        artifacts
    }
}
