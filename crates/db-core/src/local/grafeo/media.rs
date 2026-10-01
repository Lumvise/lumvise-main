#[cfg(test)]
use crate::DbError;
use crate::local::fingerprint::{fingerprint_hamming_distance, fingerprints_match_exactly};
use crate::local::grafeo::graph_rows::{
    nodes_by_label_and_property, semantic_artifacts_for_element, semantic_element_by_id,
    semantic_element_from_node,
};
use crate::{Result, SemanticArtifact, SemanticElement};
use grafeo::GrafeoDB;
#[cfg(test)]
use grafeo::Value as GrafeoValue;
use serde_json::{Value, json};

const MEDIA_SIMHASH_MAX_DISTANCE: u32 = 12;
#[cfg(test)]
const PATH_PROPERTY: &str = "path";
const PROJECT_ROOT_PROPERTY: &str = "project_root";

#[cfg(test)]
pub(crate) fn active_media_element(
    graph: &GrafeoDB,
    project_root: &str,
    target_path: &str,
) -> Result<SemanticElement> {
    let candidates = graph
        .find_nodes_by_property(PATH_PROPERTY, &GrafeoValue::from(target_path))
        .into_iter()
        .filter_map(|node_id| graph.get_node(node_id));
    active_media_element_from_nodes(candidates, project_root, target_path)
}

#[cfg(test)]
fn active_media_element_from_nodes(
    nodes: impl Iterator<Item = grafeo_core::graph::lpg::Node>,
    project_root: &str,
    target_path: &str,
) -> Result<SemanticElement> {
    let mut elements = nodes
        .filter(|node| node.has_label("SemanticElement"))
        .filter_map(|node| semantic_element_from_node(&node))
        .filter(|element| media_path_matches(element, project_root, target_path))
        .collect::<Vec<_>>();
    elements.sort_by(|left, right| left.semantic_element_id.cmp(&right.semantic_element_id));
    elements.into_iter().next().ok_or_else(|| {
        DbError::invalid_value(target_path, "active media semantic element for path")
    })
}

#[cfg(test)]
pub(crate) fn annotation_for_media_element(
    artifact: &SemanticArtifact,
    media_element: &SemanticElement,
    target_path: &str,
    anchor_selector: &str,
) -> SemanticArtifact {
    let mut artifact = artifact.clone();
    artifact.semantic_element_id = media_element.semantic_element_id.clone();
    artifact.metadata = direct_annotation_metadata(
        artifact.metadata,
        media_element,
        target_path,
        anchor_selector,
    );
    artifact
}

pub(crate) fn artifacts_for_element_with_inheritance(
    graph: &GrafeoDB,
    semantic_element_id: &str,
) -> Result<Vec<SemanticArtifact>> {
    let Some(target) = element_by_id(graph, semantic_element_id) else {
        return Ok(Vec::new());
    };
    let direct = direct_artifacts_for_element(graph, semantic_element_id);
    if !direct.is_empty() || !is_media_or_segment_element(&target) {
        return Ok(direct);
    }
    inherited_artifacts_for_media_element(graph, &target)
}

pub(crate) fn fingerprint_algorithm(element: &SemanticElement) -> Option<&str> {
    element
        .metadata
        .get("fingerprint_algorithm")
        .and_then(Value::as_str)
}

fn inherited_artifacts_for_media_element(
    graph: &GrafeoDB,
    target: &SemanticElement,
) -> Result<Vec<SemanticArtifact>> {
    let candidates = inherited_source_candidates(graph, target);
    if candidates.len() != 1 {
        return Ok(Vec::new());
    }
    let candidate = &candidates[0];
    Ok(
        direct_artifacts_for_element(graph, &candidate.element.semantic_element_id)
            .into_iter()
            .map(|artifact| inherited_artifact(artifact, target, candidate))
            .collect(),
    )
}

fn inherited_source_candidates(graph: &GrafeoDB, target: &SemanticElement) -> Vec<MediaCandidate> {
    nodes_by_label_and_property(
        graph,
        "SemanticElement",
        PROJECT_ROOT_PROPERTY,
        &target.project_root,
    )
    .iter()
    .filter_map(semantic_element_from_node)
    .filter(|source| source.semantic_element_id != target.semantic_element_id)
    .filter(|source| media_candidate_shape_matches(source, target))
    .filter(|source| !direct_artifacts_for_element(graph, &source.semantic_element_id).is_empty())
    .filter_map(|source| MediaCandidate::new(source, target))
    .collect()
}

fn media_candidate_shape_matches(source: &SemanticElement, target: &SemanticElement) -> bool {
    source.project_root == target.project_root
        && source.element_kind == target.element_kind
        && source.lifecycle == "active"
        && media_kind(source) == media_kind(target)
        && fingerprint_algorithm(source) == fingerprint_algorithm(target)
}

fn direct_artifacts_for_element(
    graph: &GrafeoDB,
    semantic_element_id: &str,
) -> Vec<SemanticArtifact> {
    semantic_artifacts_for_element(graph, semantic_element_id)
}

fn inherited_artifact(
    mut artifact: SemanticArtifact,
    target: &SemanticElement,
    candidate: &MediaCandidate,
) -> SemanticArtifact {
    artifact.semantic_element_id = target.semantic_element_id.clone();
    artifact.metadata["association_kind"] = json!("inherited");
    artifact.metadata["association_confidence"] = json!(candidate.confidence);
    artifact.metadata["association_precaution"] = json!(candidate.precaution);
    artifact.metadata["inherited_from_semantic_element_id"] =
        json!(candidate.element.semantic_element_id);
    artifact
}

#[cfg(test)]
fn direct_annotation_metadata(
    mut metadata: Value,
    media_element: &SemanticElement,
    target_path: &str,
    anchor_selector: &str,
) -> Value {
    metadata["target_path"] = json!(target_path);
    metadata["anchor_selector"] = json!(anchor_selector);
    metadata["association_kind"] = json!("direct");
    metadata["association_confidence"] = json!(100);
    metadata["association_precaution"] = json!(false);
    metadata["semantic_element_id"] = json!(media_element.semantic_element_id);
    metadata["semantic_element_type"] = json!(media_element.element_kind);
    metadata["semantic_element_name"] = json!(media_element.name);
    metadata["semantic_element_path"] = json!(media_element.path);
    metadata["semantic_element_content_fingerprint"] = json!(media_element.content_fingerprint);
    metadata["semantic_element_fingerprint_algorithm"] =
        json!(fingerprint_algorithm(media_element));
    if let Some(parent_element_id) = &media_element.parent_element_id {
        metadata["parent_element_id"] = json!(parent_element_id);
    }
    if let Some(media_kind) = media_kind(media_element) {
        metadata["media_kind"] = json!(media_kind);
    }
    metadata
}

fn element_by_id(graph: &GrafeoDB, semantic_element_id: &str) -> Option<SemanticElement> {
    semantic_element_by_id(graph, semantic_element_id)
}

#[cfg(test)]
fn media_path_matches(element: &SemanticElement, project_root: &str, target_path: &str) -> bool {
    element.project_root == project_root
        && element.path == target_path
        && element.lifecycle == "active"
        && is_top_level_media_element(element)
}

#[cfg(test)]
fn is_top_level_media_element(element: &SemanticElement) -> bool {
    media_kind(element).is_some()
}

fn media_kind(element: &SemanticElement) -> Option<&str> {
    file_media_kind(element)
}

fn is_media_or_segment_kind(kind: &str) -> bool {
    matches!(
        kind,
        "image" | "audio" | "video" | "image_region" | "audio_segment" | "video_segment"
    )
}

fn is_media_or_segment_element(element: &SemanticElement) -> bool {
    media_kind(element).is_some() || is_media_or_segment_kind(&element.element_kind)
}

fn file_media_kind(element: &SemanticElement) -> Option<&str> {
    (element.element_kind == "file")
        .then(|| element.metadata.get("media_kind").and_then(Value::as_str))
        .flatten()
        .filter(|kind| matches!(*kind, "image" | "audio" | "video"))
}

#[derive(Debug)]
struct MediaCandidate {
    element: SemanticElement,
    confidence: u8,
    precaution: bool,
}

impl MediaCandidate {
    fn new(element: SemanticElement, target: &SemanticElement) -> Option<Self> {
        let left = element.content_fingerprint.as_deref()?;
        let right = target.content_fingerprint.as_deref()?;
        if fingerprints_match_exactly(left, right) {
            return Some(Self::exact(element));
        }
        let distance = fingerprint_hamming_distance(left, right)?;
        (distance <= MEDIA_SIMHASH_MAX_DISTANCE).then(|| Self::similar(element))
    }

    fn exact(element: SemanticElement) -> Self {
        Self {
            element,
            confidence: 100,
            precaution: false,
        }
    }

    fn similar(element: SemanticElement) -> Self {
        Self {
            element,
            confidence: 90,
            precaution: true,
        }
    }
}
