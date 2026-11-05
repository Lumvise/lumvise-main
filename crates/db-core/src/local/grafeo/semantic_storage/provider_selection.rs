//! Resolves producer ownership from the current graph inside the publication plan.
use super::*;
use crate::local::grafeo::graph_reads::GrafeoResult;
use crate::local::grafeo::graph_rows::selective_elements;

pub(super) fn select_owned_partition(
    database: &grafeo::GrafeoDB,
    partition: &SemanticPartition,
    incoming: &[SemanticElement],
) -> Result<Vec<SemanticElement>> {
    let rows = partition_rows(database, partition, incoming)?;
    if partition.provider_instance_id.is_none() {
        return Ok(rows);
    }
    let identities = existing_provider_identities(database, incoming)?;
    partition.validate_ownership(&identities, incoming)?;
    Ok(rows
        .into_iter()
        .filter(|element| partition.contains_element(element))
        .collect())
}

fn existing_provider_identities(
    database: &grafeo::GrafeoDB,
    incoming: &[SemanticElement],
) -> Result<Vec<SemanticElement>> {
    // IDs are global: checking only this project would permit stealing another project's row.
    let mut node_ids = Vec::new();
    for element in incoming {
        node_ids.extend(
            database
                .find_nodes_by_property(
                    "semantic_element_id",
                    &GrafeoValue::from(element.semantic_element_id.as_str()),
                )
                .db()?,
        );
    }
    selective_elements(database, &node_ids, None, true)
}

fn partition_rows(
    database: &grafeo::GrafeoDB,
    partition: &SemanticPartition,
    incoming: &[SemanticElement],
) -> Result<Vec<SemanticElement>> {
    let root = &partition.project_root;
    let paths = &partition.replace_paths;
    let project_rows = || semantic_elements_for_project_including_inactive(database, root);
    if paths.is_empty() {
        return project_rows();
    }
    if partition_paths_are_exact(paths, incoming) {
        let paths = incoming_partition_paths(paths, incoming);
        return semantic_elements_for_paths_including_inactive(database, root, &paths);
    }
    Ok(elements_in_partition(&project_rows()?, paths))
}
