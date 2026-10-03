use super::*;
#[derive(Clone)]
pub(crate) struct ProjectionElement {
    pub(crate) semantic_element_id: String,
    pub(crate) path: String,
    pub(crate) element_kind: String,
    pub(crate) name: String,
    pub(crate) start_line: Option<i64>,
    pub(crate) end_line: Option<i64>,
    pub(crate) lifecycle: String,
    pub(crate) metadata: serde_json::Value,
}

pub(crate) struct BatchedGraphRows {
    pub(crate) all: Vec<ProjectionElement>,
    pub(crate) all_by_id: HashMap<String, ProjectionElement>,
    pub(crate) relationships: Vec<SemanticRelationship>,
    pub(crate) artifacts_by_element: HashMap<String, Vec<ProjectionArtifact>>,
}

#[derive(Clone)]
pub(crate) struct ProjectionArtifact {
    pub(crate) artifact_id: String,
    pub(crate) semantic_element_id: String,
    pub(crate) artifact_kind: String,
    pub(crate) title: String,
    pub(crate) content_ref: Option<String>,
    pub(crate) content: Option<String>,
    pub(crate) searchable_text: Option<String>,
    pub(crate) content_size_bytes: Option<usize>,
    pub(crate) metadata: serde_json::Value,
}

impl BatchedGraphRows {
    pub(super) fn snapshot(snapshot: &SemanticProjectSnapshot) -> Self {
        let all = snapshot
            .elements
            .iter()
            .filter(|element| element.lifecycle != "inactive")
            .map(ProjectionElement::from)
            .collect::<Vec<_>>();
        let all_by_id = all
            .iter()
            .map(|element| (element.semantic_element_id.clone(), element.clone()))
            .collect::<HashMap<_, _>>();
        let mut relationships = snapshot
            .relationships
            .iter()
            .filter(|edge| all_by_id.contains_key(&edge.source_element_id))
            .cloned()
            .collect::<Vec<_>>();
        relationships.sort_by_key(|edge| {
            (
                edge.source_element_id.clone(),
                edge.target_element_id.clone(),
                edge.relationship_kind.clone(),
                edge.label.clone(),
            )
        });
        let mut artifacts_by_element = HashMap::new();
        for artifact in snapshot.artifacts.iter().filter(|artifact| {
            artifact.metadata["association_kind"] != "inherited"
                && all_by_id.contains_key(&artifact.semantic_element_id)
        }) {
            artifacts_by_element
                .entry(artifact.semantic_element_id.clone())
                .or_insert_with(Vec::new)
                .push(ProjectionArtifact::from(artifact));
        }
        Self {
            all,
            all_by_id,
            relationships,
            artifacts_by_element,
        }
    }
}
impl From<&SemanticElement> for ProjectionElement {
    fn from(element: &SemanticElement) -> Self {
        Self {
            semantic_element_id: element.semantic_element_id.clone(),
            path: element.path.clone(),
            element_kind: element.element_kind.clone(),
            name: element.name.clone(),
            start_line: element.start_line.filter(|line| *line >= 0),
            end_line: element.end_line.filter(|line| *line >= 0),
            lifecycle: element.lifecycle.clone(),
            metadata: element.metadata.clone(),
        }
    }
}
impl From<&SemanticArtifact> for ProjectionArtifact {
    fn from(artifact: &SemanticArtifact) -> Self {
        Self {
            artifact_id: artifact.artifact_id.clone(),
            semantic_element_id: artifact.semantic_element_id.clone(),
            artifact_kind: artifact.artifact_kind.clone(),
            title: artifact.title.clone(),
            content_ref: artifact.content_ref.clone().filter(|text| !text.is_empty()),
            content: artifact.content.clone().filter(|text| !text.is_empty()),
            searchable_text: artifact
                .searchable_text
                .clone()
                .filter(|text| !text.is_empty()),
            content_size_bytes: artifact.content_size_bytes,
            metadata: artifact.metadata.clone(),
        }
    }
}
