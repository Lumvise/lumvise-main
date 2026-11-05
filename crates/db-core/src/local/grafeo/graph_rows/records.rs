use super::*;
use crate::local::grafeo::committed_columns::CommittedPropertyRows;

#[cfg(test)]
pub(crate) fn semantic_node_id_index(
    database: &GraphTransaction<'_>,
    project_root: &str,
) -> Result<BTreeMap<String, NodeId>> {
    let mut node_ids = BTreeMap::new();
    for node_id in
        database.find_nodes_by_property(PROJECT_ROOT_PROPERTY, &GrafeoValue::from(project_root))?
    {
        let Some(node) = database.get_node(node_id)? else {
            continue;
        };
        if !node.has_label("SemanticElement") {
            continue;
        }
        if let Some(semantic_id) = string_property(&node, SEMANTIC_ELEMENT_ID_PROPERTY) {
            node_ids.insert(semantic_id, node.id);
        }
    }
    node_ids.extend(database.semantic_node_ids());
    Ok(node_ids)
}

pub(crate) struct CompleteProjectRows {
    pub(crate) elements: Vec<SemanticElement>,
    pub(crate) relationships: Vec<SemanticRelationship>,
    pub(crate) artifacts: Vec<SemanticArtifact>,
}

const COMPLETE_ELEMENT_PROPERTIES: &[&str] = &[
    "project_root",
    "semantic_element_id",
    "semantic_source_id",
    "path",
    "element_kind",
    "name",
    "parent_element_id",
    "revision_dependencies_json",
    "content_fingerprint",
    "start_line",
    "end_line",
    "lifecycle",
    "match_confidence",
    "simhash_distance",
    "matched_at",
    "precaution",
    "metadata_json",
    "revision_selected",
];

const COMPLETE_ARTIFACT_PROPERTIES: &[&str] = &[
    "artifact_id",
    "semantic_element_id",
    "artifact_kind",
    "title",
    "content_ref",
    "content",
    "searchable_text",
    "content_size_bytes",
    "dependencies_json",
    "metadata_json",
    "revision_selected",
];
pub(crate) fn semantic_element_by_id_selective(
    database: &GrafeoDB,
    semantic_element_id: &str,
) -> Result<Option<SemanticElement>> {
    let node_ids = database
        .find_nodes_by_property(
            SEMANTIC_ELEMENT_ID_PROPERTY,
            &GrafeoValue::from(semantic_element_id),
        )
        .db()?;
    Ok(
        selective_current_elements(database, &node_ids, None, false)?
            .into_iter()
            .next(),
    )
}

/// Tests current project membership without requiring an artifact's historical owner body.
/// Example: vector search uses this projection predicate before scoring candidates.
pub(crate) fn artifact_is_current_in_project(
    database: &GrafeoDB,
    root: &str,
    artifact: &str,
) -> Result<bool> {
    for node in node_ids_by_label_and_property(
        database,
        "SemanticArtifact",
        ARTIFACT_ID_PROPERTY,
        artifact,
    )? {
        if current_artifact_root(database, node)?.as_deref() == Some(root) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn current_artifact_root(database: &GrafeoDB, node: NodeId) -> Result<Option<String>> {
    let Some(node) = database.get_node(node).db()? else {
        return Ok(None);
    };
    if node.get_property("revision_selected") == Some(&GrafeoValue::from(false)) {
        return Ok(None);
    }
    // Managed artifacts carry project scope because their canonical owner can be collected.
    if let Some(root) = string_property(&node, PROJECT_ROOT_PROPERTY) {
        return Ok(Some(root));
    }
    let Some(owner) = string_property(&node, SEMANTIC_ELEMENT_ID_PROPERTY) else {
        return Ok(None);
    };
    Ok(semantic_element_by_id_selective(database, &owner)?.map(|owner| owner.project_root))
}

const COMPLETE_EDGE_PROPERTIES: &[&str] = &[
    "project_root",
    "source_element_id",
    "target_element_id",
    "relationship_kind",
    "label",
    "artifact_id",
    "semantic_element_id",
    "metadata_json",
];

pub(crate) fn complete_project_rows(
    database: &GrafeoDB,
    project_root: &str,
) -> Result<CompleteProjectRows> {
    let element_ids = database
        .find_nodes_by_property(PROJECT_ROOT_PROPERTY, &GrafeoValue::from(project_root))
        .db()?;
    let elements = selective_elements(database, &element_ids, Some(project_root), true)?;
    let (relationships, artifact_ids) = selective_adjacency(database, &element_ids, project_root)?;
    let artifacts = selective_artifacts(database, &elements, &artifact_ids, None)?;
    Ok(CompleteProjectRows {
        elements,
        relationships,
        artifacts,
    })
}

pub(crate) fn project_artifacts_selective(
    database: &GrafeoDB,
    project_root: &str,
    artifact_namespace: Option<&str>,
) -> Result<Vec<SemanticArtifact>> {
    let snapshot = database.committed_snapshot()?;
    let view = snapshot.view();
    let artifact_nodes = snapshot.nodes_by_label("SemanticArtifact", view).db()?;
    let mut managed = Vec::new();
    for id in &artifact_nodes {
        if let Some(node) = database.get_node(*id).db()?
            && string_property(&node, "project_root").as_deref() == Some(project_root)
            && node.get_property("revision_selected") != Some(&GrafeoValue::from(false))
        {
            managed.push(*id);
        }
    }
    if !managed.is_empty() {
        return selective_artifacts(database, &[], &managed, artifact_namespace);
    }
    // Artifact search needs only owners of existing artifacts, not every source
    // element in a large project. Keep the same project-membership predicate.
    let mut owners = semantic_owner_ids(database, &artifact_nodes)?;
    let mut in_project = HashSet::new();
    for owner in &owners {
        let ids = database
            .find_nodes_by_property(
                SEMANTIC_ELEMENT_ID_PROPERTY,
                &GrafeoValue::from(owner.as_str()),
            )
            .db()?;
        for id in ids {
            let root = snapshot
                .node_property(id, PROJECT_ROOT_PROPERTY, view)
                .db()?;
            if root.and_then(|value| value_string(&value)).as_deref() == Some(project_root) {
                in_project.insert(owner.clone());
                break;
            }
        }
    }
    owners.retain(|owner| in_project.contains(owner));
    selective_artifacts_for_owners(database, owners, artifact_namespace)
}

pub(crate) fn artifacts_by_ids_selective(
    database: &GrafeoDB,
    artifact_ids: &HashSet<String>,
) -> Result<Vec<SemanticArtifact>> {
    let mut node_ids = Vec::new();
    for id in artifact_ids {
        node_ids.extend(
            database
                .find_nodes_by_property(ARTIFACT_ID_PROPERTY, &GrafeoValue::from(id.as_str()))
                .db()?,
        );
    }
    selective_artifacts(database, &[], &node_ids, None)
}
pub(super) fn selective_node_property_batch(
    snapshot: &Snapshot,
    node_ids: &[NodeId],
    key: &str,
) -> Result<Vec<Option<GrafeoValue>>> {
    #[cfg(test)]
    record_selective_node_batch_call();
    // GRAFEO06-REVIEW(3): read through the snapshot's committed view; 0.5 read the live store.
    snapshot
        .node_property_batch(node_ids, &key.into(), snapshot.view())
        .db()?
        .committed_nodes(snapshot, node_ids)
}

pub(crate) fn elements_by_ids_selective(
    database: &GrafeoDB,
    project_root: &str,
    semantic_element_ids: &HashSet<String>,
    include_inactive: bool,
) -> Result<Vec<SemanticElement>> {
    let mut node_ids = Vec::new();
    for id in semantic_element_ids {
        node_ids.extend(
            database
                .find_nodes_by_property(
                    SEMANTIC_ELEMENT_ID_PROPERTY,
                    &GrafeoValue::from(id.as_str()),
                )
                .db()?,
        );
    }
    selective_current_elements(database, &node_ids, Some(project_root), include_inactive)
}

/// Unions equality-index hits across the three exact identity keys Knowledge's
/// cross-project inheritance admits candidates through - raw content
/// fingerprint, normalized `(kind, name)`, and normalized `(kind, file name)`
/// - into one deduplicated candidate element set spanning every project.
pub(crate) fn candidate_elements_by_identity_keys(
    database: &GrafeoDB,
    content_fingerprints: &HashSet<String>,
    kind_name_keys: &HashSet<String>,
    kind_file_name_keys: &HashSet<String>,
) -> Result<Vec<SemanticElement>> {
    let mut node_ids = HashSet::new();
    for value in content_fingerprints {
        node_ids.extend(
            database
                .find_nodes_by_property(
                    CONTENT_FINGERPRINT_PROPERTY,
                    &GrafeoValue::from(value.as_str()),
                )
                .db()?,
        );
    }
    for value in kind_name_keys {
        node_ids.extend(
            database
                .find_nodes_by_property(
                    IDENTITY_KIND_NAME_PROPERTY,
                    &GrafeoValue::from(value.as_str()),
                )
                .db()?,
        );
    }
    for value in kind_file_name_keys {
        node_ids.extend(
            database
                .find_nodes_by_property(
                    IDENTITY_KIND_FILE_NAME_PROPERTY,
                    &GrafeoValue::from(value.as_str()),
                )
                .db()?,
        );
    }
    let node_ids = node_ids.into_iter().collect::<Vec<_>>();
    selective_elements(database, &node_ids, None, true)
}

pub(crate) fn selective_elements(
    database: &GrafeoDB,
    node_ids: &[NodeId],
    project_root: Option<&str>,
    include_inactive: bool,
) -> Result<Vec<SemanticElement>> {
    selective_elements_in_view(
        database,
        node_ids,
        project_root,
        include_inactive,
        ElementReadView::Retained,
    )
}

pub(crate) fn selective_current_elements(
    database: &GrafeoDB,
    node_ids: &[NodeId],
    project_root: Option<&str>,
    include_inactive: bool,
) -> Result<Vec<SemanticElement>> {
    selective_elements_in_view(
        database,
        node_ids,
        project_root,
        include_inactive,
        ElementReadView::Current,
    )
}

enum ElementReadView {
    Current,
    Retained,
}

fn selective_elements_in_view(
    database: &GrafeoDB,
    node_ids: &[NodeId],
    project_root: Option<&str>,
    include_inactive: bool,
    view: ElementReadView,
) -> Result<Vec<SemanticElement>> {
    let snapshot = database.committed_snapshot()?;
    let keys = COMPLETE_ELEMENT_PROPERTIES
        .iter()
        .map(|key| (*key).into())
        .collect::<Vec<_>>();
    #[cfg(test)]
    record_selective_node_batch_call();
    // GRAFEO06-REVIEW(4): committed ReadView batch; 0.5 read live columns then masked by epoch.
    let rows = snapshot
        .node_properties_selective_batch(node_ids, &keys, snapshot.view())
        .db()?
        .committed_nodes(&snapshot, node_ids)?;
    Ok(node_ids
        .iter()
        .enumerate()
        .filter_map(|(row, _)| {
            let properties = rows.get(row)?;
            if matches!(view, ElementReadView::Current)
                && properties.get("revision_selected") == Some(&GrafeoValue::from(false))
            {
                return None;
            }
            element_from_properties(|key| rows.get(row).and_then(|properties| properties.get(key)))
        })
        .filter(|element| project_root.is_none_or(|root| element.project_root == root))
        .filter(|element| include_inactive || element.lifecycle != "inactive")
        .collect())
}

pub(crate) fn element_from_properties<'a>(
    property: impl Fn(&str) -> Option<&'a GrafeoValue>,
) -> Option<SemanticElement> {
    Some(SemanticElement {
        project_root: property("project_root").and_then(value_string)?,
        semantic_element_id: property("semantic_element_id").and_then(value_string)?,
        semantic_source_id: property("semantic_source_id").and_then(value_string)?,
        path: property("path").and_then(value_string)?,
        element_kind: property("element_kind").and_then(value_string)?,
        name: property("name").and_then(value_string)?,
        parent_element_id: property("parent_element_id").and_then(value_non_empty),
        revision_dependencies: match property("revision_dependencies_json") {
            None => Vec::new(),
            Some(GrafeoValue::String(encoded)) => serde_json::from_str(encoded).ok()?,
            _ => return None,
        },
        content_fingerprint: property("content_fingerprint").and_then(value_non_empty),
        start_line: property("start_line").and_then(value_non_negative_i64),
        end_line: property("end_line").and_then(value_non_negative_i64),
        lifecycle: property("lifecycle")
            .and_then(value_string)
            .unwrap_or_else(|| "active".to_string()),
        match_evidence: match_evidence_from_properties(&property),
        metadata: property("metadata_json")
            .map(value_json)
            .unwrap_or(serde_json::Value::Null),
    })
}

pub(super) fn match_evidence_from_properties<'a>(
    property: &impl Fn(&str) -> Option<&'a GrafeoValue>,
) -> Option<SemanticMatchEvidence> {
    Some(SemanticMatchEvidence {
        match_confidence: u8::try_from(
            property("match_confidence").and_then(value_non_negative_i64)?,
        )
        .ok()?,
        simhash_distance: property("simhash_distance")
            .and_then(value_non_negative_i64)
            .and_then(|value| u32::try_from(value).ok()),
        matched_at: property("matched_at").and_then(value_non_empty)?,
        precaution: property("precaution").and_then(value_non_empty),
    })
}

pub(super) fn selective_adjacency(
    database: &GrafeoDB,
    element_nodes: &[NodeId],
    project_root: &str,
) -> Result<(Vec<SemanticRelationship>, Vec<NodeId>)> {
    let snapshot = database.committed_snapshot()?;
    let view = snapshot.view();
    let mut adjacency = Vec::new();
    for source in element_nodes {
        // GRAFEO06-REVIEW(5): committed adjacency; 0.5 edges_from listed the live store's edges.
        for (target, edge) in snapshot
            .edges(*source, Direction::Outgoing, &[], view)
            .db()?
        {
            adjacency.push((*source, target, edge));
        }
    }
    let edge_ids = adjacency
        .iter()
        .map(|(_, _, edge)| *edge)
        .collect::<Vec<_>>();
    let edge_keys = COMPLETE_EDGE_PROPERTIES
        .iter()
        .map(|key| (*key).into())
        .collect::<Vec<_>>();
    #[cfg(test)]
    record_selective_edge_batch_call();
    let properties = snapshot
        .edge_properties_selective_batch(&edge_ids, &edge_keys, view)
        .db()?
        .committed_edges(&snapshot, &edge_ids)?;
    let mut relationships = Vec::new();
    let mut artifact_nodes = Vec::new();
    for ((_, target, _), properties) in adjacency.into_iter().zip(properties) {
        if properties
            .get("artifact_id")
            .and_then(value_string)
            .is_some()
        {
            artifact_nodes.push(target);
            continue;
        }
        if let Some(relationship) = relationship_from_properties(|key| properties.get(key))
            .filter(|relationship| relationship.project_root == project_root)
        {
            relationships.push(relationship);
        }
    }
    Ok((relationships, artifact_nodes))
}

pub(super) fn selective_relationships(
    database: &GrafeoDB,
    edge_ids: &[EdgeId],
) -> Result<Vec<SemanticRelationship>> {
    let snapshot = database.committed_snapshot()?;
    let keys = COMPLETE_EDGE_PROPERTIES
        .iter()
        .map(|key| (*key).into())
        .collect::<Vec<_>>();
    #[cfg(test)]
    record_selective_edge_batch_call();
    Ok(snapshot
        .edge_properties_selective_batch(edge_ids, &keys, snapshot.view())
        .db()?
        .committed_edges(&snapshot, edge_ids)?
        .iter()
        .filter_map(|row| relationship_from_properties(|key| row.get(key)))
        .collect())
}

fn relationship_from_properties<'a>(
    property: impl Fn(&str) -> Option<&'a GrafeoValue>,
) -> Option<SemanticRelationship> {
    Some(SemanticRelationship {
        project_root: property("project_root").and_then(value_string)?,
        source_element_id: property("source_element_id").and_then(value_string)?,
        target_element_id: property("target_element_id").and_then(value_string)?,
        relationship_kind: property("relationship_kind").and_then(value_string)?,
        label: property("label").and_then(value_string)?,
        metadata: property("metadata_json")
            .map(value_json)
            .unwrap_or(serde_json::Value::Null),
    })
}

pub(super) fn semantic_owner_ids(
    database: &GrafeoDB,
    node_ids: &[NodeId],
) -> Result<HashSet<String>> {
    let snapshot = database.committed_snapshot()?;
    Ok(
        selective_node_property_batch(&snapshot, node_ids, SEMANTIC_ELEMENT_ID_PROPERTY)?
            .into_iter()
            .filter_map(|value| value.and_then(|value| value_string(&value)))
            .collect(),
    )
}

pub(super) fn selective_artifacts_for_owners(
    database: &GrafeoDB,
    owner_ids: HashSet<String>,
    artifact_namespace: Option<&str>,
) -> Result<Vec<SemanticArtifact>> {
    let snapshot = database.committed_snapshot()?;
    // Owner equality is indexed; unrelated artifacts never need hydration.
    let mut node_ids = Vec::new();
    for owner in &owner_ids {
        let owned = database
            .find_nodes_by_property(
                SEMANTIC_ELEMENT_ID_PROPERTY,
                &GrafeoValue::from(owner.as_str()),
            )
            .db()?;
        for id in owned {
            if database
                .get_node(id)
                .db()?
                .is_some_and(|node| node.has_label("SemanticArtifact"))
            {
                node_ids.push(id);
            }
        }
    }
    let keys = COMPLETE_ARTIFACT_PROPERTIES
        .iter()
        .map(|key| (*key).into())
        .collect::<Vec<_>>();
    #[cfg(test)]
    record_selective_node_batch_call();
    let rows = snapshot
        .node_properties_selective_batch(&node_ids, &keys, snapshot.view())
        .db()?
        .committed_nodes(&snapshot, &node_ids)?;
    let mut seen = HashSet::new();
    Ok(node_ids
        .iter()
        .enumerate()
        .filter_map(|(row, _)| {
            if rows
                .get(row)
                .and_then(|properties| properties.get("revision_selected"))
                == Some(&GrafeoValue::from(false))
            {
                return None;
            }
            let artifact = artifact_from_properties(|key| {
                rows.get(row).and_then(|properties| properties.get(key))
            })?;
            (owner_ids.contains(&artifact.semantic_element_id)
                && artifact.metadata["association_kind"] != "inherited"
                && artifact_namespace
                    .is_none_or(|namespace| artifact.metadata[namespace].is_object())
                && seen.insert(artifact.artifact_id.clone()))
            .then_some(artifact)
        })
        .collect())
}

pub(super) fn selective_artifacts(
    database: &GrafeoDB,
    elements: &[SemanticElement],
    edge_artifact_nodes: &[NodeId],
    artifact_namespace: Option<&str>,
) -> Result<Vec<SemanticArtifact>> {
    let owner_ids = elements
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect::<HashSet<_>>();
    let mut node_ids = edge_artifact_nodes.to_vec();
    for element_id in &owner_ids {
        node_ids.extend(
            database
                .find_nodes_by_property(
                    SEMANTIC_ELEMENT_ID_PROPERTY,
                    &GrafeoValue::from(*element_id),
                )
                .db()?,
        );
    }
    let mut seen_nodes = HashSet::new();
    node_ids.retain(|node_id| seen_nodes.insert(*node_id));
    let snapshot = database.committed_snapshot()?;
    let keys = COMPLETE_ARTIFACT_PROPERTIES
        .iter()
        .map(|key| (*key).into())
        .collect::<Vec<_>>();
    #[cfg(test)]
    record_selective_node_batch_call();
    let rows = snapshot
        .node_properties_selective_batch(&node_ids, &keys, snapshot.view())
        .db()?
        .committed_nodes(&snapshot, &node_ids)?;
    let mut seen = HashSet::new();
    Ok(node_ids
        .iter()
        .enumerate()
        .filter_map(|(row, _)| {
            let artifact = artifact_from_properties(|key| {
                rows.get(row).and_then(|properties| properties.get(key))
            })?;
            if !owner_ids.is_empty() && !owner_ids.contains(artifact.semantic_element_id.as_str()) {
                return None;
            }
            if artifact.metadata["association_kind"] == "inherited"
                || artifact_namespace
                    .is_some_and(|namespace| !artifact.metadata[namespace].is_object())
            {
                return None;
            }
            seen.insert(artifact.artifact_id.clone())
                .then_some(artifact)
        })
        .collect())
}

pub(super) fn artifact_from_properties<'a>(
    property: impl Fn(&str) -> Option<&'a GrafeoValue>,
) -> Option<SemanticArtifact> {
    Some(SemanticArtifact {
        artifact_id: property("artifact_id").and_then(value_string)?,
        semantic_element_id: property("semantic_element_id").and_then(value_string)?,
        artifact_kind: property("artifact_kind").and_then(value_string)?,
        title: property("title").and_then(value_string)?,
        content_ref: property("content_ref").and_then(value_non_empty),
        content: property("content").and_then(value_non_empty),
        searchable_text: property("searchable_text").and_then(value_non_empty),
        content_size_bytes: property("content_size_bytes")
            .and_then(value_non_negative_i64)
            .and_then(|value| usize::try_from(value).ok()),
        dependencies: property("dependencies_json")
            .map(value_json)
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default(),
        metadata: property("metadata_json")
            .map(value_json)
            .unwrap_or(serde_json::Value::Null),
    })
}

pub(crate) fn value_string(value: &GrafeoValue) -> Option<String> {
    match value {
        GrafeoValue::String(value) => Some(value.to_string()),
        GrafeoValue::Int64(value) => Some(value.to_string()),
        GrafeoValue::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn value_non_empty(value: &GrafeoValue) -> Option<String> {
    value_string(value).filter(|value| !value.is_empty())
}

pub(crate) fn value_non_negative_i64(value: &GrafeoValue) -> Option<i64> {
    match value {
        GrafeoValue::Int64(value) if *value >= 0 => Some(*value),
        GrafeoValue::String(value) => value.parse().ok().filter(|value| *value >= 0),
        _ => None,
    }
}

pub(super) fn value_json(value: &GrafeoValue) -> serde_json::Value {
    value_non_empty(value)
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or(serde_json::Value::Null)
}
#[cfg(test)]
pub(crate) fn semantic_elements_for_project(
    database: &GrafeoDB,
    project_root: &str,
) -> Result<Vec<SemanticElement>> {
    Ok(
        semantic_elements_for_project_including_inactive(database, project_root)?
            .into_iter()
            .filter(|element| element.lifecycle != "inactive")
            .collect(),
    )
}

pub(crate) fn semantic_elements_for_project_including_inactive(
    database: &GrafeoDB,
    project_root: &str,
) -> Result<Vec<SemanticElement>> {
    let snapshot = database.committed_snapshot()?;
    let semantic_node_ids = snapshot
        .nodes_by_label("SemanticElement", snapshot.view())
        .db()?
        .into_iter()
        .collect::<HashSet<_>>();
    let node_ids = database
        .find_nodes_by_property(PROJECT_ROOT_PROPERTY, &GrafeoValue::from(project_root))
        .db()?
        .into_iter()
        .filter(|node_id| semantic_node_ids.contains(node_id))
        .collect::<Vec<_>>();
    selective_elements(database, &node_ids, Some(project_root), true)
}

pub(crate) fn semantic_element_by_id(
    database: &GrafeoDB,
    semantic_element_id: &str,
) -> Result<Option<SemanticElement>> {
    let node_ids = database
        .find_nodes_by_property(
            SEMANTIC_ELEMENT_ID_PROPERTY,
            &GrafeoValue::from(semantic_element_id),
        )
        .db()?;
    for node_id in node_ids {
        if let Some(node) = database.get_node(node_id).db()?
            && node.has_label("SemanticElement")
            && element_node_is_active(&node)
        {
            return Ok(semantic_element_from_node(&node));
        }
    }
    Ok(None)
}

pub(crate) fn semantic_artifacts_for_elements(
    database: &GrafeoDB,
    semantic_element_ids: &HashSet<String>,
) -> Result<Vec<SemanticArtifact>> {
    let mut artifacts = BTreeMap::new();
    for artifact in semantic_artifacts_for_elements_from_edges(database, semantic_element_ids)? {
        artifacts.insert(artifact.artifact_id.clone(), artifact);
    }
    for artifact in semantic_artifacts_for_elements_by_property(database, semantic_element_ids)? {
        artifacts
            .entry(artifact.artifact_id.clone())
            .or_insert(artifact);
    }
    Ok(artifacts.into_values().collect())
}

pub(super) fn semantic_artifacts_for_elements_by_property(
    database: &GrafeoDB,
    semantic_element_ids: &HashSet<String>,
) -> Result<Vec<SemanticArtifact>> {
    selective_artifacts_for_owners(database, semantic_element_ids.clone(), None)
}

pub(crate) fn semantic_artifacts_for_element(
    database: &GrafeoDB,
    semantic_element_id: &str,
) -> Result<Vec<SemanticArtifact>> {
    let mut element_ids = HashSet::with_capacity(1);
    element_ids.insert(semantic_element_id.to_string());
    semantic_artifacts_for_elements(database, &element_ids)
}

pub(crate) fn semantic_artifact_by_id(
    database: &GrafeoDB,
    artifact_id: &str,
) -> Result<Option<SemanticArtifact>> {
    let node_ids = database
        .find_nodes_by_property(ARTIFACT_ID_PROPERTY, &GrafeoValue::from(artifact_id))
        .db()?;
    for node_id in node_ids {
        if let Some(node) = database.get_node(node_id).db()?
            && node.has_label("SemanticArtifact")
        {
            return Ok(semantic_artifact_from_node(&node));
        }
    }
    Ok(None)
}

/// Looks up nodes carrying `label` via the `property = value` index instead of a full scan.
pub(crate) fn node_ids_by_label_and_property(
    database: &GrafeoDB,
    label: &str,
    property: &str,
    value: &str,
) -> Result<Vec<NodeId>> {
    // Bulk project operations need identities, not cloned metadata/vector properties.
    if property == PROJECT_ROOT_PROPERTY {
        let snapshot = database.committed_snapshot()?;
        let labeled: HashSet<_> = snapshot
            .nodes_by_label(label, snapshot.view())
            .db()?
            .into_iter()
            .collect();
        return Ok(database
            .find_nodes_by_property(property, &GrafeoValue::from(value))
            .db()?
            .into_iter()
            .filter(|id| labeled.contains(id))
            .collect());
    }
    Ok(
        nodes_by_label_and_property(database, label, property, value)?
            .into_iter()
            .filter(|node| string_property(node, property).as_deref() == Some(value))
            .map(|node| node.id)
            .collect(),
    )
}

pub(crate) fn nodes_by_label_and_property(
    database: &GrafeoDB,
    label: &str,
    property: &str,
    value: &str,
) -> Result<Vec<grafeo_core::graph::lpg::Node>> {
    let mut nodes = Vec::new();
    for node_id in database
        .find_nodes_by_property(property, &GrafeoValue::from(value))
        .db()?
    {
        if let Some(node) = database.get_node(node_id).db()?
            && node.has_label(label)
        {
            nodes.push(node);
        }
    }
    Ok(nodes)
}

#[cfg(test)]
pub(crate) fn delete_nodes_by_property(
    database: &GraphTransaction<'_>,
    label: &str,
    property: &str,
    value: &str,
) -> Result<usize> {
    let mut node_ids = Vec::new();
    for node_id in database.find_nodes_by_property(property, &GrafeoValue::from(value))? {
        if let Some(node) = database.get_node(node_id)?
            && node.has_label(label)
            && string_property(&node, property).as_deref() == Some(value)
        {
            node_ids.push(node.id);
        }
    }
    let count = node_ids.len();
    for node_id in node_ids {
        database.delete_node(node_id)?;
    }
    Ok(count)
}

pub(super) fn relationship_edge_touches_element(
    edge: &grafeo_core::graph::lpg::Edge,
    semantic_element_id: &str,
) -> bool {
    edge_string_property(edge, "source_element_id").as_deref() == Some(semantic_element_id)
        || edge_string_property(edge, "target_element_id").as_deref() == Some(semantic_element_id)
}
pub(super) fn semantic_artifacts_for_elements_from_edges(
    database: &GrafeoDB,
    semantic_element_ids: &HashSet<String>,
) -> Result<Vec<SemanticArtifact>> {
    let snapshot = database.committed_snapshot()?;
    let view = snapshot.view();
    let mut artifacts = Vec::new();
    for node in active_element_nodes(database, semantic_element_ids)? {
        for (_, edge_id) in snapshot.edges(node, Direction::Outgoing, &[], view).db()? {
            let Some(edge) = database.get_edge(edge_id).db()? else {
                continue;
            };
            if edge.edge_type != SEMANTIC_ARTIFACT_EDGE_TYPE {
                continue;
            }
            let Some(target) = database.get_node(edge.dst).db()? else {
                continue;
            };
            if target.has_label("SemanticArtifact")
                && target.get_property("revision_selected") != Some(&GrafeoValue::from(false))
                && let Some(artifact) = semantic_artifact_from_node(&target)
                && artifact.metadata["association_kind"] != "inherited"
            {
                artifacts.push(artifact);
            }
        }
    }
    Ok(artifacts)
}

pub(super) fn active_element_nodes(
    database: &GrafeoDB,
    semantic_element_ids: &HashSet<String>,
) -> Result<Vec<NodeId>> {
    let mut nodes = Vec::new();
    for id in semantic_element_ids {
        let owned = database
            .find_nodes_by_property(
                SEMANTIC_ELEMENT_ID_PROPERTY,
                &GrafeoValue::from(id.as_str()),
            )
            .db()?;
        for node_id in owned {
            if database
                .get_node(node_id)
                .db()?
                .is_some_and(|node| node.has_label("SemanticElement"))
            {
                nodes.push(node_id);
            }
        }
    }
    let snapshot = database.committed_snapshot()?;
    let rows = snapshot
        .node_properties_selective_batch(
            &nodes,
            &["active".into(), "lifecycle".into()],
            snapshot.view(),
        )
        .db()?
        .committed_nodes(&snapshot, &nodes)?;
    Ok(nodes
        .into_iter()
        .zip(rows)
        .filter(|(_, row)| active_properties(row.get("active"), row.get("lifecycle")))
        .map(|(node, _)| node)
        .collect())
}

pub(super) fn semantic_element_node_id(
    database: &GrafeoDB,
    semantic_element_id: &str,
) -> Result<Option<NodeId>> {
    let direct_id = semantic_element_node_id_for(semantic_element_id);
    if semantic_element_id_matches(database, direct_id, semantic_element_id)? {
        return Ok(Some(direct_id));
    }
    let node_ids = database
        .find_nodes_by_property(
            SEMANTIC_ELEMENT_ID_PROPERTY,
            &GrafeoValue::from(semantic_element_id),
        )
        .db()?;
    for node_id in node_ids {
        if database
            .get_node(node_id)
            .db()?
            .is_some_and(|node| node.has_label("SemanticElement") && element_node_is_active(&node))
        {
            return Ok(Some(node_id));
        }
    }
    Ok(None)
}

#[cfg(test)]
pub(super) fn transaction_semantic_element_node_id(
    database: &GraphTransaction<'_>,
    semantic_element_id: &str,
) -> Result<Option<NodeId>> {
    if let Some(node_id) = database.semantic_node_id(semantic_element_id) {
        return Ok(Some(node_id));
    }
    let node_ids = database.find_nodes_by_property(
        SEMANTIC_ELEMENT_ID_PROPERTY,
        &GrafeoValue::from(semantic_element_id),
    )?;
    for node_id in node_ids {
        if database
            .get_node(node_id)?
            .is_some_and(|node| node.has_label("SemanticElement"))
        {
            return Ok(Some(node_id));
        }
    }
    Ok(None)
}

pub(super) fn element_node_is_active(node: &grafeo_core::graph::lpg::Node) -> bool {
    active_properties(
        node.get_property(ACTIVE_PROPERTY),
        node.get_property("lifecycle"),
    )
}

pub(super) fn active_properties(
    active: Option<&GrafeoValue>,
    lifecycle: Option<&GrafeoValue>,
) -> bool {
    match active {
        Some(GrafeoValue::Bool(active)) => *active,
        Some(GrafeoValue::String(active)) => active == "true",
        Some(GrafeoValue::Int64(active)) => *active != 0,
        Some(_) => false,
        None => lifecycle.and_then(value_string).as_deref() != Some("inactive"),
    }
}

pub(super) fn semantic_element_id_matches(
    database: &GrafeoDB,
    node_id: NodeId,
    semantic_element_id: &str,
) -> Result<bool> {
    Ok(database.get_node(node_id).db()?.is_some_and(|node| {
        node.has_label("SemanticElement")
            && element_node_is_active(&node)
            && string_property(&node, SEMANTIC_ELEMENT_ID_PROPERTY).as_deref()
                == Some(semantic_element_id)
    }))
}
