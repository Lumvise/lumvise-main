#[cfg(test)]
use crate::DbError;
#[cfg(test)]
use crate::domain::graph_views::media_kind;
use crate::domain::graph_views::{MediaAssociationSource, artifact_inheritance_from_source};
use crate::local::grafeo::graph_rows::{
    nodes_by_label_and_property, semantic_artifacts_for_element, semantic_element_by_id,
    semantic_element_from_node,
};
use crate::{Result, SemanticArtifact, SemanticElement};
use grafeo::GrafeoDB;
#[cfg(test)]
use grafeo::Value as GrafeoValue;
#[cfg(test)]
use serde_json::{Value, json};

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

struct GrafeoMediaSource<'graph>(&'graph GrafeoDB);
pub(crate) fn artifacts_for_element_with_inheritance(
    graph: &GrafeoDB,
    id: &str,
) -> Result<Vec<SemanticArtifact>> {
    artifact_inheritance_from_source(&GrafeoMediaSource(graph), id)
}
impl MediaAssociationSource for GrafeoMediaSource<'_> {
    fn element(&self, id: &str) -> Option<SemanticElement> {
        semantic_element_by_id(self.0, id)
    }
    fn candidates(&self, root: &str) -> Vec<SemanticElement> {
        nodes_by_label_and_property(self.0, "SemanticElement", PROJECT_ROOT_PROPERTY, root)
            .iter()
            .filter_map(semantic_element_from_node)
            .collect()
    }
    fn artifacts(&self, id: &str) -> Vec<SemanticArtifact> {
        semantic_artifacts_for_element(self.0, id)
    }
}
#[cfg(test)]
use crate::domain::fingerprint::fingerprint_algorithm;
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
