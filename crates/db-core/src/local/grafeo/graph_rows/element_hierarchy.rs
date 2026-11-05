use super::{
    CONTAINS_RELATIONSHIP_KIND, PARENT_ELEMENT_ID_PROPERTY, PATH_PROPERTY,
    SEMANTIC_ELEMENT_ID_PROPERTY, insert_native_relationships,
};
use crate::local::grafeo::graph_reads::GrafeoResult;
use crate::local::grafeo::graph_row_projection::{semantic_element_from_node, string_property};
use crate::{Result, SemanticElement, SemanticRelationship};
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
) -> Result<Vec<SemanticElement>> {
    let mut elements = Vec::new();
    for path in paths {
        elements.extend(semantic_elements_for_path(database, project_root, path)?);
    }
    Ok(elements)
}

fn semantic_elements_for_path(
    database: &GrafeoDB,
    project_root: &str,
    path: &str,
) -> Result<Vec<SemanticElement>> {
    let mut elements = Vec::new();
    for node_id in database
        .find_nodes_by_property(PATH_PROPERTY, &GrafeoValue::from(path))
        .db()?
    {
        let Some(node) = database.get_node(node_id).db()? else {
            continue;
        };
        if !node.has_label("SemanticElement") {
            continue;
        }
        let Some(mut element) = semantic_element_from_node(&node) else {
            continue;
        };
        if element.project_root == project_root && element.path == path {
            if element.parent_element_id.is_none() {
                element.parent_element_id =
                    parent_element_id_for(database, &element.semantic_element_id)?;
            }
            elements.push(element);
        }
    }
    Ok(elements)
}

pub(super) fn parent_element_id_for(
    database: &GrafeoDB,
    semantic_element_id: &str,
) -> Result<Option<String>> {
    let mut property_parent = None;
    for node_id in database
        .find_nodes_by_property(
            SEMANTIC_ELEMENT_ID_PROPERTY,
            &GrafeoValue::from(semantic_element_id),
        )
        .db()?
    {
        if let Some(node) = database.get_node(node_id).db()?
            && node.has_label("SemanticElement")
        {
            property_parent =
                string_property(&node, PARENT_ELEMENT_ID_PROPERTY).filter(|id| !id.is_empty());
            break;
        }
    }
    if property_parent.is_some() {
        return Ok(property_parent);
    }
    let mut relationships = BTreeMap::new();
    insert_native_relationships(database, semantic_element_id, &mut relationships)?;
    Ok(relationships
        .into_values()
        .find(|relationship| {
            relationship_is_contains(relationship)
                && relationship.target_element_id == semantic_element_id
        })
        .map(|relationship| relationship.source_element_id))
}
