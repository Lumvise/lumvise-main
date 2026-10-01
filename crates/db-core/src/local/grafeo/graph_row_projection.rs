use crate::{
    ArtifactTextVector, Result, SemanticArtifact, SemanticElement, SemanticMatchEvidence,
    SemanticRelationship, StoredArtifactTextVector, StoredSemanticElementNameVector,
};
use grafeo::Value as GrafeoValue;
use serde_json::Value;

const PROJECT_ROOT_PROPERTY: &str = "project_root";
const SEMANTIC_ELEMENT_ID_PROPERTY: &str = "semantic_element_id";
const ARTIFACT_ID_PROPERTY: &str = "artifact_id";
const PATH_PROPERTY: &str = "path";
const RELATIONSHIP_KIND_PROPERTY: &str = "relationship_kind";
const RELATIONSHIP_LABEL_PROPERTY: &str = "label";
const RELATIONSHIP_METADATA_PROPERTY: &str = "metadata_json";
const SOURCE_ELEMENT_ID_PROPERTY: &str = "source_element_id";
const TARGET_ELEMENT_ID_PROPERTY: &str = "target_element_id";

pub(crate) fn semantic_element_from_node(
    node: &grafeo_core::graph::lpg::Node,
) -> Option<SemanticElement> {
    Some(SemanticElement {
        project_root: string_property(node, PROJECT_ROOT_PROPERTY)?,
        semantic_element_id: string_property(node, SEMANTIC_ELEMENT_ID_PROPERTY)?,
        semantic_source_id: string_property(node, "semantic_source_id")?,
        path: string_property(node, PATH_PROPERTY)?,
        element_kind: string_property(node, "element_kind")?,
        name: string_property(node, "name")?,
        parent_element_id: non_empty_property(node, "parent_element_id"),
        content_fingerprint: non_empty_property(node, "content_fingerprint"),
        start_line: non_negative_i64_property(node, "start_line"),
        end_line: non_negative_i64_property(node, "end_line"),
        lifecycle: string_property(node, "lifecycle").unwrap_or_else(|| "active".to_string()),
        match_evidence: match_evidence_from_node(node),
        metadata: json_property(node, "metadata_json"),
    })
}

pub(crate) fn semantic_artifact_from_node(
    node: &grafeo_core::graph::lpg::Node,
) -> Option<SemanticArtifact> {
    Some(SemanticArtifact {
        artifact_id: string_property(node, ARTIFACT_ID_PROPERTY)?,
        semantic_element_id: string_property(node, SEMANTIC_ELEMENT_ID_PROPERTY)?,
        artifact_kind: string_property(node, "artifact_kind")?,
        title: string_property(node, "title")?,
        content_ref: non_empty_property(node, "content_ref"),
        content: non_empty_property(node, "content"),
        searchable_text: non_empty_property(node, "searchable_text"),
        content_size_bytes: non_negative_i64_property(node, "content_size_bytes")
            .and_then(|value| usize::try_from(value).ok()),
        dependencies: json_property(node, "dependencies_json")
            .as_array()
            .and_then(|value| serde_json::from_value(Value::Array(value.clone())).ok())
            .unwrap_or_default(),
        metadata: json_property(node, "metadata_json"),
    })
}

pub(crate) fn semantic_relationship_from_edge(
    edge: &grafeo_core::graph::lpg::Edge,
) -> Option<SemanticRelationship> {
    Some(SemanticRelationship {
        project_root: edge_string_property(edge, PROJECT_ROOT_PROPERTY)?,
        source_element_id: edge_string_property(edge, SOURCE_ELEMENT_ID_PROPERTY)?,
        target_element_id: edge_string_property(edge, TARGET_ELEMENT_ID_PROPERTY)?,
        relationship_kind: edge_string_property(edge, RELATIONSHIP_KIND_PROPERTY)?,
        label: edge_string_property(edge, RELATIONSHIP_LABEL_PROPERTY)?,
        metadata: edge_json_property(edge, RELATIONSHIP_METADATA_PROPERTY),
    })
}

pub(crate) fn artifact_vector_from_node(
    node: &grafeo_core::graph::lpg::Node,
) -> Option<StoredArtifactTextVector> {
    Some(StoredArtifactTextVector {
        artifact_id: string_property(node, ARTIFACT_ID_PROPERTY)?,
        semantic_element_id: string_property(node, SEMANTIC_ELEMENT_ID_PROPERTY)?,
        source_text: string_property(node, "source_text")?,
        vector: ArtifactTextVector {
            engine_id: string_property(node, "engine_id")?,
            model: non_empty_property(node, "model"),
            dimensions: usize::try_from(non_negative_i64_property(node, "dimensions")?).ok()?,
            vector: vector_property(node)?,
            normalized: bool_property(node, "normalized"),
            metadata: json_property(node, "metadata_json"),
        },
    })
}

pub(crate) fn element_name_vector_from_node(
    node: &grafeo_core::graph::lpg::Node,
) -> Option<StoredSemanticElementNameVector> {
    Some(StoredSemanticElementNameVector {
        semantic_element_id: string_property(node, SEMANTIC_ELEMENT_ID_PROPERTY)?,
        project_root: string_property(node, PROJECT_ROOT_PROPERTY)?,
        source_text: string_property(node, "source_text")?,
        vector: ArtifactTextVector {
            engine_id: string_property(node, "engine_id")?,
            model: non_empty_property(node, "model"),
            dimensions: usize::try_from(non_negative_i64_property(node, "dimensions")?).ok()?,
            vector: vector_property(node)?,
            normalized: bool_property(node, "normalized"),
            metadata: json_property(node, "metadata_json"),
        },
    })
}

fn vector_property(node: &grafeo_core::graph::lpg::Node) -> Option<Vec<f32>> {
    match node.get_property("vector") {
        Some(GrafeoValue::Vector(vector)) => Some(vector.to_vec()),
        _ => None,
    }
}

pub(crate) fn json_property(node: &grafeo_core::graph::lpg::Node, key: &str) -> Value {
    non_empty_property(node, key)
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or(Value::Null)
}

pub(crate) fn non_empty_property(
    node: &grafeo_core::graph::lpg::Node,
    key: &str,
) -> Option<String> {
    string_property(node, key).filter(|value| !value.is_empty())
}

pub(crate) fn non_negative_i64_property(
    node: &grafeo_core::graph::lpg::Node,
    key: &str,
) -> Option<i64> {
    i64_property(node, key).filter(|value| *value >= 0)
}

pub(crate) fn string_property(node: &grafeo_core::graph::lpg::Node, key: &str) -> Option<String> {
    match node.get_property(key) {
        Some(GrafeoValue::String(value)) => Some(value.to_string()),
        Some(GrafeoValue::Int64(value)) => Some(value.to_string()),
        Some(GrafeoValue::Bool(value)) => Some(value.to_string()),
        _ => None,
    }
}

pub(crate) fn edge_string_property(
    edge: &grafeo_core::graph::lpg::Edge,
    key: &str,
) -> Option<String> {
    match edge.get_property(key) {
        Some(GrafeoValue::String(value)) => Some(value.to_string()),
        Some(GrafeoValue::Int64(value)) => Some(value.to_string()),
        Some(GrafeoValue::Bool(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn edge_json_property(edge: &grafeo_core::graph::lpg::Edge, key: &str) -> Value {
    edge_non_empty_property(edge, key)
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or(Value::Null)
}

fn edge_non_empty_property(edge: &grafeo_core::graph::lpg::Edge, key: &str) -> Option<String> {
    edge_string_property(edge, key).filter(|value| !value.is_empty())
}

pub(crate) fn i64_property(node: &grafeo_core::graph::lpg::Node, key: &str) -> Option<i64> {
    match node.get_property(key) {
        Some(GrafeoValue::Int64(value)) => Some(*value),
        Some(GrafeoValue::String(value)) => value.parse().ok(),
        _ => None,
    }
}

pub(crate) fn bool_property(node: &grafeo_core::graph::lpg::Node, key: &str) -> bool {
    match node.get_property(key) {
        Some(GrafeoValue::Bool(value)) => *value,
        Some(GrafeoValue::String(value)) => value == "true",
        Some(GrafeoValue::Int64(value)) => *value != 0,
        _ => false,
    }
}

pub(crate) fn artifact_vector_props(
    artifact: &SemanticArtifact,
    source_text: &str,
    vector: &ArtifactTextVector,
) -> Result<Vec<(&'static str, GrafeoValue)>> {
    vector_props(
        vec![
            (
                "artifact_id",
                GrafeoValue::from(artifact.artifact_id.clone()),
            ),
            (
                "semantic_element_id",
                GrafeoValue::from(artifact.semantic_element_id.clone()),
            ),
        ],
        source_text,
        vector,
    )
}

pub(crate) fn element_name_vector_props(
    element: &SemanticElement,
    source_text: &str,
    vector: &ArtifactTextVector,
) -> Result<Vec<(&'static str, GrafeoValue)>> {
    vector_props(
        vec![
            (
                SEMANTIC_ELEMENT_ID_PROPERTY,
                GrafeoValue::from(element.semantic_element_id.clone()),
            ),
            (
                PROJECT_ROOT_PROPERTY,
                GrafeoValue::from(element.project_root.clone()),
            ),
        ],
        source_text,
        vector,
    )
}

fn vector_props(
    mut properties: Vec<(&'static str, GrafeoValue)>,
    source_text: &str,
    vector: &ArtifactTextVector,
) -> Result<Vec<(&'static str, GrafeoValue)>> {
    properties.extend([
        ("source_text", GrafeoValue::from(source_text.to_string())),
        ("engine_id", GrafeoValue::from(vector.engine_id.clone())),
        (
            "model",
            GrafeoValue::from(vector.model.clone().unwrap_or_default()),
        ),
        ("dimensions", GrafeoValue::from(vector.dimensions as i64)),
        ("normalized", GrafeoValue::from(vector.normalized)),
        ("vector", GrafeoValue::Vector(vector.vector.clone().into())),
        (
            "metadata_json",
            GrafeoValue::from(serde_json::to_string(&vector.metadata)?),
        ),
    ]);
    Ok(properties)
}

fn match_evidence_from_node(node: &grafeo_core::graph::lpg::Node) -> Option<SemanticMatchEvidence> {
    let confidence = non_negative_i64_property(node, "match_confidence")?;
    Some(SemanticMatchEvidence {
        match_confidence: u8::try_from(confidence).ok()?,
        simhash_distance: non_negative_i64_property(node, "simhash_distance")
            .and_then(|value| u32::try_from(value).ok()),
        matched_at: non_empty_property(node, "matched_at")?,
        precaution: non_empty_property(node, "precaution"),
    })
}

pub(crate) fn storage_alias_properties(metadata: &Value) -> Vec<(&'static str, GrafeoValue)> {
    let alias = &metadata["storage_alias"];
    vec![
        (
            "storage_alias_ref",
            GrafeoValue::from(alias_string(alias, "alias_ref")),
        ),
        (
            "storage_alias_target_uri",
            GrafeoValue::from(alias_string(alias, "target_uri")),
        ),
        (
            "storage_alias_media_type",
            GrafeoValue::from(alias_string(alias, "media_type")),
        ),
        (
            "storage_alias_content_hash",
            GrafeoValue::from(alias_string(alias, "content_hash")),
        ),
        (
            "storage_alias_size_bytes",
            GrafeoValue::from(alias_i64(alias, "size_bytes")),
        ),
    ]
}

fn alias_string(alias: &Value, key: &str) -> String {
    alias
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn alias_i64(alias: &Value, key: &str) -> i64 {
    alias.get(key).and_then(Value::as_i64).unwrap_or(-1)
}
