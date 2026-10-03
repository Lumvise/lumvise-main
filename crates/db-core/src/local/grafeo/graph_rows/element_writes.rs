use super::*;

pub(crate) fn prepare_element_upsert(
    database: &GrafeoDB,
    element: &SemanticElement,
) -> Result<ElementUpsertPlan> {
    Ok(ElementUpsertPlan {
        existing_node_ids: node_ids_by_label_and_property(
            database,
            "SemanticElement",
            SEMANTIC_ELEMENT_ID_PROPERTY,
            &element.semantic_element_id,
        ),
        properties: element_properties(
            element,
            element.match_evidence.as_ref(),
            serde_json::to_string(&element.metadata)?,
        ),
    })
}

pub(crate) fn apply_element_upsert(
    database: &GraphTransaction<'_>,
    element: &SemanticElement,
    commit_version: i64,
    plan: ElementUpsertPlan,
) -> Result<()> {
    if let Some(node_id) = plan.existing_node_ids.first() {
        for (property, value) in plan.properties {
            database.set_node_property(*node_id, property, value)?;
        }
        set_element_change_state(database, *node_id, element, commit_version)?;
        return Ok(());
    }
    database.create_semantic_node(
        &element.semantic_element_id,
        element_properties_with_change_state(element, commit_version)?,
    )?;
    Ok(())
}

pub(crate) fn prepare_element_name_vector_replace(
    database: &GrafeoDB,
    element: &SemanticElement,
    source_text: &str,
    vector: &ArtifactTextVector,
) -> Result<VectorReplacePlan> {
    Ok(VectorReplacePlan {
        node_ids: node_ids_by_label_and_property(
            database,
            "SemanticElementNameVector",
            SEMANTIC_ELEMENT_ID_PROPERTY,
            &element.semantic_element_id,
        ),
        properties: element_name_vector_props(element, source_text, vector)?,
    })
}

pub(crate) fn prepare_artifact_vector_replace(
    database: &GrafeoDB,
    artifact: &SemanticArtifact,
    source_text: &str,
    vector: &ArtifactTextVector,
) -> Result<VectorReplacePlan> {
    Ok(VectorReplacePlan {
        node_ids: node_ids_by_label_and_property(
            database,
            "SemanticArtifactVector",
            ARTIFACT_ID_PROPERTY,
            &artifact.artifact_id,
        ),
        properties: artifact_vector_props(artifact, source_text, vector)?,
    })
}

pub(crate) fn apply_element_name_vector_replace(
    database: &GraphTransaction<'_>,
    plan: VectorReplacePlan,
) -> Result<()> {
    apply_vector_replace(database, "SemanticElementNameVector", plan)
}

pub(crate) fn apply_artifact_vector_replace(
    database: &GraphTransaction<'_>,
    plan: VectorReplacePlan,
) -> Result<()> {
    apply_vector_replace(database, "SemanticArtifactVector", plan)
}

pub(super) fn apply_vector_replace(
    database: &GraphTransaction<'_>,
    label: &str,
    plan: VectorReplacePlan,
) -> Result<()> {
    for node_id in plan.node_ids {
        database.delete_node(node_id);
    }
    database.create_node_with_props(&[label], plan.properties)?;
    Ok(())
}

pub(super) fn dependency_target_node(
    database: &GrafeoDB,
    dependency: &ArtifactDependency,
) -> Option<(String, NodeId)> {
    match &dependency.target {
        ArtifactDependencyTarget::SemanticElement {
            semantic_element_id,
        } => node_ids_by_label_and_property(
            database,
            "SemanticElement",
            SEMANTIC_ELEMENT_ID_PROPERTY,
            semantic_element_id,
        )
        .into_iter()
        .next()
        .map(|node_id| ("semantic_element".into(), node_id)),
        ArtifactDependencyTarget::Artifact { artifact_id } => node_ids_by_label_and_property(
            database,
            "SemanticArtifact",
            ARTIFACT_ID_PROPERTY,
            artifact_id,
        )
        .into_iter()
        .next()
        .map(|node_id| ("artifact".into(), node_id)),
    }
}

#[cfg(test)]
pub(crate) fn upsert_element_node(
    database: &GraphTransaction<'_>,
    element: &SemanticElement,
    commit_version: i64,
) -> Result<()> {
    let existing = database
        .find_nodes_by_property(
            SEMANTIC_ELEMENT_ID_PROPERTY,
            &GrafeoValue::from(element.semantic_element_id.as_str()),
        )
        .into_iter()
        .filter(|node_id| {
            database
                .get_node(*node_id)
                .is_some_and(|node| node.has_label("SemanticElement"))
        })
        .collect::<Vec<_>>();
    if let [node_id] = existing.as_slice() {
        for (property, value) in element_properties(
            element,
            element.match_evidence.as_ref(),
            serde_json::to_string(&element.metadata)?,
        ) {
            database.set_node_property(*node_id, property, value)?;
        }
        set_element_change_state(database, *node_id, element, commit_version)?;
        return Ok(());
    }
    delete_nodes_by_property(
        database,
        "SemanticElement",
        SEMANTIC_ELEMENT_ID_PROPERTY,
        &element.semantic_element_id,
    );
    insert_element_node(database, element, commit_version)
}

#[cfg(test)]
pub(crate) fn insert_element_node(
    database: &GraphTransaction<'_>,
    element: &SemanticElement,
    commit_version: i64,
) -> Result<()> {
    database.create_semantic_node(
        &element.semantic_element_id,
        element_properties_with_change_state(element, commit_version)?,
    )?;
    Ok(())
}

pub(crate) fn semantic_element_node_id_for(semantic_element_id: &str) -> NodeId {
    NodeId::new(stable_node_hash(semantic_element_id))
}

pub(super) fn element_properties(
    element: &SemanticElement,
    evidence: Option<&SemanticMatchEvidence>,
    metadata_json: String,
) -> Vec<(&'static str, GrafeoValue)> {
    let mut properties = vec![
        (
            PROJECT_ROOT_PROPERTY,
            GrafeoValue::from(element.project_root.clone()),
        ),
        (
            SEMANTIC_ELEMENT_ID_PROPERTY,
            GrafeoValue::from(element.semantic_element_id.clone()),
        ),
        (
            "semantic_source_id",
            GrafeoValue::from(element.semantic_source_id.clone()),
        ),
        (
            PARENT_ELEMENT_ID_PROPERTY,
            GrafeoValue::from(element.parent_element_id.clone().unwrap_or_default()),
        ),
        (PATH_PROPERTY, GrafeoValue::from(element.path.clone())),
        (
            "element_kind",
            GrafeoValue::from(element.element_kind.clone()),
        ),
        ("name", GrafeoValue::from(element.name.clone())),
        (
            CONTENT_FINGERPRINT_PROPERTY,
            GrafeoValue::from(element.content_fingerprint.clone().unwrap_or_default()),
        ),
        (
            IDENTITY_KIND_NAME_PROPERTY,
            GrafeoValue::from(identity_kind_name_value(element)),
        ),
        (
            IDENTITY_KIND_FILE_NAME_PROPERTY,
            GrafeoValue::from(identity_kind_file_name_value(element)),
        ),
        (
            "start_line",
            GrafeoValue::from(element.start_line.unwrap_or(-1)),
        ),
        (
            "end_line",
            GrafeoValue::from(element.end_line.unwrap_or(-1)),
        ),
        ("lifecycle", GrafeoValue::from(element.lifecycle.clone())),
        (
            "match_confidence",
            GrafeoValue::from(evidence.map_or(-1, |item| i64::from(item.match_confidence))),
        ),
        (
            "simhash_distance",
            GrafeoValue::from(
                evidence
                    .and_then(|item| item.simhash_distance.map(i64::from))
                    .unwrap_or(-1),
            ),
        ),
        (
            "matched_at",
            GrafeoValue::from(evidence.map_or("", |item| item.matched_at.as_str())),
        ),
        (
            "precaution",
            GrafeoValue::from(
                evidence
                    .and_then(|item| item.precaution.as_deref())
                    .unwrap_or(""),
            ),
        ),
        ("metadata_json", GrafeoValue::from(metadata_json)),
    ];
    properties.extend(storage_alias_properties(&element.metadata));
    properties
}

pub(super) fn element_properties_with_change_state(
    element: &SemanticElement,
    commit_version: i64,
) -> Result<Vec<(&'static str, GrafeoValue)>> {
    let mut properties = element_properties(
        element,
        element.match_evidence.as_ref(),
        serde_json::to_string(&element.metadata)?,
    );
    properties.extend(element_change_properties(element, commit_version));
    Ok(properties)
}

pub(super) fn element_change_properties(
    element: &SemanticElement,
    commit_version: i64,
) -> [(&'static str, GrafeoValue); 3] {
    let active = element.lifecycle != "inactive";
    [
        (
            LAST_CHANGED_REVISION_PROPERTY,
            GrafeoValue::from(commit_version),
        ),
        (ACTIVE_PROPERTY, GrafeoValue::from(active)),
        (
            DELETED_AT_PROPERTY,
            GrafeoValue::from(if active { -1 } else { commit_version }),
        ),
    ]
}

pub(super) fn set_element_change_state(
    database: &GraphTransaction<'_>,
    node_id: NodeId,
    element: &SemanticElement,
    commit_version: i64,
) -> Result<()> {
    for (property, value) in element_change_properties(element, commit_version) {
        database.set_node_property(node_id, property, value)?;
    }
    Ok(())
}

pub(super) fn stable_node_hash(value: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in b"lumvise.semantic_element:" {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash.max(1)
}

pub(crate) fn prepare_project_relationship_reset(
    database: &GrafeoDB,
    project_root: &str,
) -> ProjectRelationshipResetPlan {
    let element_node_ids = node_ids_by_label_and_property(
        database,
        "SemanticElement",
        PROJECT_ROOT_PROPERTY,
        project_root,
    );
    let relationship_edge_ids = element_node_ids
        .iter()
        .flat_map(|node_id| database.store().edges_from(*node_id, Direction::Outgoing))
        .filter_map(|(_, edge_id)| database.get_edge(edge_id))
        .filter(|edge| semantic_relationship_from_edge(edge).is_some())
        .map(|edge| edge.id)
        .collect();
    ProjectRelationshipResetPlan {
        relationship_edge_ids,
    }
}

pub(crate) fn apply_project_relationship_reset(
    database: &GraphTransaction<'_>,
    plan: ProjectRelationshipResetPlan,
) {
    for edge_id in plan.relationship_edge_ids {
        database.delete_edge(edge_id);
    }
}

pub(crate) fn prepare_artifact_deletion(
    database: &GrafeoDB,
    artifact_id: &str,
) -> ArtifactDeletionPlan {
    let artifact_node_ids = node_ids_by_label_and_property(
        database,
        "SemanticArtifact",
        ARTIFACT_ID_PROPERTY,
        artifact_id,
    );
    let artifact = artifact_node_ids
        .iter()
        .filter_map(|node_id| database.get_node(*node_id))
        .find_map(|node| semantic_artifact_from_node(&node));
    let owner_node_id = artifact
        .as_ref()
        .and_then(|artifact| semantic_element_node_id(database, &artifact.semantic_element_id));
    let owner_element = owner_node_id
        .and_then(|node_id| database.get_node(node_id))
        .and_then(|node| semantic_element_from_node(&node));
    ArtifactDeletionPlan {
        artifact_node_ids,
        artifact_vector_node_ids: node_ids_by_label_and_property(
            database,
            "SemanticArtifactVector",
            ARTIFACT_ID_PROPERTY,
            artifact_id,
        ),
        artifact_edge_ids: database
            .iter_edges()
            .filter(|edge| edge.edge_type == SEMANTIC_ARTIFACT_EDGE_TYPE)
            .filter(|edge| {
                edge_string_property(edge, ARTIFACT_ID_PROPERTY).as_deref() == Some(artifact_id)
            })
            .map(|edge| edge.id)
            .collect(),
        owner_node_id,
        owner_element,
        artifact,
    }
}

pub(crate) fn apply_artifact_deletion(
    database: &GraphTransaction<'_>,
    commit_version: i64,
    plan: ArtifactDeletionPlan,
) -> Option<SemanticElement> {
    let owner = plan.owner_element.clone();
    if let Some(owner_node_id) = plan.owner_node_id {
        database
            .set_node_property(
                owner_node_id,
                LAST_CHANGED_REVISION_PROPERTY,
                GrafeoValue::from(commit_version),
            )
            .expect("planned semantic artifact owner remains available");
    }
    for edge_id in plan.artifact_edge_ids {
        database.delete_edge(edge_id);
    }
    for node_id in plan.artifact_node_ids {
        database.delete_node(node_id);
    }
    for node_id in plan.artifact_vector_node_ids {
        database.delete_node(node_id);
    }
    owner
}

pub(crate) fn prepare_element_deactivation(
    database: &GrafeoDB,
    semantic_element_id: &str,
) -> ElementDeactivationPlan {
    let element_node_ids = node_ids_by_label_and_property(
        database,
        "SemanticElement",
        SEMANTIC_ELEMENT_ID_PROPERTY,
        semantic_element_id,
    );
    let active_element_node_ids = element_node_ids
        .iter()
        .copied()
        .filter(|node_id| {
            database
                .get_node(*node_id)
                .is_some_and(|node| element_node_is_active(&node))
        })
        .collect::<Vec<_>>();
    let project_root = active_element_node_ids
        .iter()
        .filter_map(|node_id| database.get_node(*node_id))
        .find_map(|node| string_property(&node, PROJECT_ROOT_PROPERTY))
        .unwrap_or_default();
    let entity_kind = active_element_node_ids
        .iter()
        .filter_map(|node_id| database.get_node(*node_id))
        .find_map(|node| string_property(&node, "element_kind"))
        .unwrap_or_default();
    let artifact_node_ids = node_ids_by_label_and_property(
        database,
        "SemanticArtifact",
        SEMANTIC_ELEMENT_ID_PROPERTY,
        semantic_element_id,
    );
    let artifacts = artifact_node_ids
        .iter()
        .filter_map(|node_id| database.get_node(*node_id))
        .filter_map(|node| semantic_artifact_from_node(&node))
        .map(|artifact| (artifact.artifact_id.clone(), artifact))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect::<Vec<_>>();
    let artifact_ids = artifacts
        .iter()
        .map(|artifact| artifact.artifact_id.as_str())
        .collect::<BTreeSet<_>>();
    let artifact_vector_node_ids = artifact_ids
        .iter()
        .flat_map(|artifact_id| {
            node_ids_by_label_and_property(
                database,
                "SemanticArtifactVector",
                ARTIFACT_ID_PROPERTY,
                artifact_id,
            )
        })
        .collect();
    ElementDeactivationPlan {
        exists: !active_element_node_ids.is_empty(),
        element_node_ids: active_element_node_ids,
        element_vector_node_ids: node_ids_by_label_and_property(
            database,
            "SemanticElementNameVector",
            SEMANTIC_ELEMENT_ID_PROPERTY,
            semantic_element_id,
        ),
        relationship_edge_ids: database
            .iter_edges()
            .filter(|edge| relationship_edge_touches_element(edge, semantic_element_id))
            .map(|edge| edge.id)
            .collect(),
        artifact_node_ids,
        artifact_vector_node_ids,
        artifacts,
        project_root,
        entity_kind,
    }
}

pub(crate) fn apply_element_deactivation(
    database: &GraphTransaction<'_>,
    commit_version: i64,
    plan: ElementDeactivationPlan,
) -> bool {
    let deactivated = !plan.element_node_ids.is_empty();
    for edge_id in plan.relationship_edge_ids {
        database.delete_edge(edge_id);
    }
    for node_id in plan.element_node_ids {
        database
            .set_node_property(node_id, "lifecycle", GrafeoValue::from("inactive"))
            .expect("planned semantic element remains available");
        database
            .set_node_property(node_id, ACTIVE_PROPERTY, GrafeoValue::from(false))
            .expect("planned semantic element remains available");
        database
            .set_node_property(
                node_id,
                DELETED_AT_PROPERTY,
                GrafeoValue::from(commit_version),
            )
            .expect("planned semantic element remains available");
        database
            .set_node_property(
                node_id,
                LAST_CHANGED_REVISION_PROPERTY,
                GrafeoValue::from(commit_version),
            )
            .expect("planned semantic element remains available");
    }
    for node_id in plan.element_vector_node_ids {
        database.delete_node(node_id);
    }
    for node_id in plan.artifact_node_ids {
        database.delete_node(node_id);
    }
    for node_id in plan.artifact_vector_node_ids {
        database.delete_node(node_id);
    }
    deactivated
}
