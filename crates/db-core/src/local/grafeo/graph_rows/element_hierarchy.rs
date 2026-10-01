use super::{
    CONTAINS_RELATIONSHIP_KIND, PARENT_ELEMENT_ID_PROPERTY, PATH_PROPERTY,
    SEMANTIC_ELEMENT_ID_PROPERTY, insert_native_relationships,
};
use crate::local::grafeo::graph_row_projection::{semantic_element_from_node, string_property};
use crate::{SemanticElement, SemanticRelationship};
use grafeo::{GrafeoDB, Value as GrafeoValue};
use std::collections::BTreeMap;
fn relationship_is_contains(relationship: &SemanticRelationship) -> bool {
    relationship.relationship_kind == CONTAINS_RELATIONSHIP_KIND
        || relationship.label == CONTAINS_RELATIONSHIP_KIND
}

pub(crate) fn semantic_elements_for_paths_including_inactive(
    database: &GrafeoDB,
    project_root: &str,
    paths: &[String],
) -> Vec<SemanticElement> {
    paths
        .iter()
        .flat_map(|path| semantic_elements_for_path(database, project_root, path))
        .collect()
}

fn semantic_elements_for_path(
    database: &GrafeoDB,
    project_root: &str,
    path: &str,
) -> Vec<SemanticElement> {
    database
        .find_nodes_by_property(PATH_PROPERTY, &GrafeoValue::from(path))
        .into_iter()
        .filter_map(|node_id| database.get_node(node_id))
        .filter(|node| node.has_label("SemanticElement"))
        .filter_map(|node| semantic_element_from_node(&node))
        .filter(|element| element.project_root == project_root && element.path == path)
        .map(|mut element| {
            if element.parent_element_id.is_none() {
                element.parent_element_id =
                    parent_element_id_for(database, &element.semantic_element_id);
            }
            element
        })
        .collect()
}

pub(super) fn parent_element_id_for(
    database: &GrafeoDB,
    semantic_element_id: &str,
) -> Option<String> {
    let property_parent = database
        .find_nodes_by_property(
            SEMANTIC_ELEMENT_ID_PROPERTY,
            &GrafeoValue::from(semantic_element_id),
        )
        .into_iter()
        .filter_map(|node_id| database.get_node(node_id))
        .find(|node| node.has_label("SemanticElement"))
        .and_then(|node| string_property(&node, PARENT_ELEMENT_ID_PROPERTY))
        .filter(|parent_id| !parent_id.is_empty());
    if property_parent.is_some() {
        return property_parent;
    }
    let mut relationships = BTreeMap::new();
    insert_native_relationships(database, semantic_element_id, &mut relationships);
    relationships
        .into_values()
        .find(|relationship| {
            relationship_is_contains(relationship)
                && relationship.target_element_id == semantic_element_id
        })
        .map(|relationship| relationship.source_element_id)
}
