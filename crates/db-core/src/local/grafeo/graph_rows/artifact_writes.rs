use super::*;

pub(crate) fn prepare_artifact_upsert(
    database: &GrafeoDB,
    artifact: &SemanticArtifact,
    vector: Option<(&str, &ArtifactTextVector)>,
) -> Result<ArtifactUpsertPlan> {
    let owner_node_ids = node_ids_by_label_and_property(
        database,
        "SemanticElement",
        SEMANTIC_ELEMENT_ID_PROPERTY,
        &artifact.semantic_element_id,
    )
    .into_iter()
    .filter(|node_id| {
        database
            .get_node(*node_id)
            .is_some_and(|node| element_node_is_active(&node))
    })
    .collect::<Vec<_>>();
    let [owner_node_id] = owner_node_ids.as_slice() else {
        return Err(DbError::invalid_value(
            &artifact.semantic_element_id,
            "exactly one existing semantic element for artifact",
        ));
    };
    let owner_element = database
        .get_node(*owner_node_id)
        .and_then(|node| semantic_element_from_node(&node))
        .ok_or_else(|| {
            DbError::invalid_value(
                &artifact.semantic_element_id,
                "semantic artifact owner semantic element",
            )
        })?;
    validate_artifact_dependencies(&artifact.artifact_id, &artifact.dependencies)
        .map_err(|error| DbError::invalid_value(&artifact.artifact_id, error))?;
    let dependency_targets = artifact
        .dependencies
        .iter()
        .map(|dependency| {
            dependency_target_node(database, dependency)
                .map(|(_, node_id)| (dependency.clone(), node_id))
                .ok_or_else(|| {
                    DbError::invalid_value(
                        dependency.target_id(),
                        "existing artifact dependency target",
                    )
                })
        })
        .collect::<Result<Vec<_>>>()?;
    let artifact_node_ids = node_ids_by_label_and_property(
        database,
        "SemanticArtifact",
        ARTIFACT_ID_PROPERTY,
        &artifact.artifact_id,
    );
    // Artifact updates must scale with their own links, not the whole project's graph.
    let dependency_edge_ids = artifact_node_ids
        .iter()
        .flat_map(|node_id| database.store().edges_from(*node_id, Direction::Outgoing))
        .filter_map(|(_, edge_id)| database.get_edge(edge_id))
        .filter(|edge| edge.edge_type == ARTIFACT_DEPENDENCY_EDGE_TYPE)
        .map(|edge| edge.id)
        .collect::<Vec<_>>();
    let inbound_dependency_rebinds = artifact_node_ids
        .iter()
        .flat_map(|node_id| database.store().edges_from(*node_id, Direction::Incoming))
        .filter_map(|(_, edge_id)| database.get_edge(edge_id))
        .filter(|edge| edge.edge_type == ARTIFACT_DEPENDENCY_EDGE_TYPE)
        .filter_map(|edge| {
            Some(ArtifactDependencyRebind {
                source_node_id: edge.src,
                target_kind: edge_string_property(&edge, ARTIFACT_DEPENDENCY_TARGET_KIND_PROPERTY)?,
                target_id: edge_string_property(&edge, ARTIFACT_DEPENDENCY_TARGET_ID_PROPERTY)?,
            })
        })
        .collect::<Vec<_>>();
    let artifact_edge_ids = artifact_node_ids
        .iter()
        .flat_map(|node_id| database.store().edges_from(*node_id, Direction::Incoming))
        .filter_map(|(_, edge_id)| database.get_edge(edge_id))
        .filter(|edge| edge.edge_type == SEMANTIC_ARTIFACT_EDGE_TYPE)
        .filter(|edge| {
            edge_string_property(edge, ARTIFACT_ID_PROPERTY).as_deref()
                == Some(&artifact.artifact_id)
        })
        .map(|edge| edge.id)
        .collect();
    Ok(ArtifactUpsertPlan {
        artifact_node_ids,
        artifact_edge_ids,
        dependency_edge_ids,
        dependency_targets,
        inbound_dependency_rebinds,
        vector_node_ids: node_ids_by_label_and_property(
            database,
            "SemanticArtifactVector",
            ARTIFACT_ID_PROPERTY,
            &artifact.artifact_id,
        ),
        owner_node_id: *owner_node_id,
        owner_element,
        artifact_properties: artifact_properties(artifact)?,
        vector_properties: vector
            .map(|(source_text, vector)| artifact_vector_props(artifact, source_text, vector))
            .transpose()?,
    })
}

pub(crate) fn apply_artifact_upsert(
    database: &GraphTransaction<'_>,
    artifact: &SemanticArtifact,
    commit_version: i64,
    plan: ArtifactUpsertPlan,
) -> Result<SemanticElement> {
    let owner = plan.owner_element.clone();
    database
        .set_node_property(
            plan.owner_node_id,
            LAST_CHANGED_REVISION_PROPERTY,
            GrafeoValue::from(commit_version),
        )
        .expect("planned semantic artifact owner remains available");
    for edge_id in plan.artifact_edge_ids {
        database.delete_edge(edge_id);
    }
    for edge_id in plan.dependency_edge_ids {
        database.delete_edge(edge_id);
    }
    for node_id in plan.artifact_node_ids {
        database.delete_node(node_id);
    }
    for node_id in plan.vector_node_ids {
        database.delete_node(node_id);
    }
    let artifact_node_id =
        database.create_node_with_props(&["SemanticArtifact"], plan.artifact_properties)?;
    database.create_edge_with_props(
        plan.owner_node_id,
        artifact_node_id,
        SEMANTIC_ARTIFACT_EDGE_TYPE,
        [
            (
                SEMANTIC_ELEMENT_ID_PROPERTY,
                GrafeoValue::from(artifact.semantic_element_id.clone()),
            ),
            (
                ARTIFACT_ID_PROPERTY,
                GrafeoValue::from(artifact.artifact_id.clone()),
            ),
        ],
    )?;
    for (dependency, target_node_id) in plan.dependency_targets {
        let target_kind = match dependency.target {
            ArtifactDependencyTarget::SemanticElement { .. } => "semantic_element",
            ArtifactDependencyTarget::Artifact { .. } => "artifact",
        };
        database.create_edge_with_props(
            artifact_node_id,
            target_node_id,
            ARTIFACT_DEPENDENCY_EDGE_TYPE,
            [
                (
                    ARTIFACT_DEPENDENCY_TARGET_KIND_PROPERTY,
                    GrafeoValue::from(target_kind),
                ),
                (
                    ARTIFACT_DEPENDENCY_TARGET_ID_PROPERTY,
                    GrafeoValue::from(dependency.target_id().to_owned()),
                ),
            ],
        )?;
    }
    for rebind in plan.inbound_dependency_rebinds {
        database.create_edge_with_props(
            rebind.source_node_id,
            artifact_node_id,
            ARTIFACT_DEPENDENCY_EDGE_TYPE,
            [
                (
                    ARTIFACT_DEPENDENCY_TARGET_KIND_PROPERTY,
                    GrafeoValue::from(rebind.target_kind),
                ),
                (
                    ARTIFACT_DEPENDENCY_TARGET_ID_PROPERTY,
                    GrafeoValue::from(rebind.target_id),
                ),
            ],
        )?;
    }
    if let Some(properties) = plan.vector_properties {
        database.create_node_with_props(&["SemanticArtifactVector"], properties)?;
    }
    Ok(owner)
}
#[cfg(test)]
pub(crate) fn upsert_artifact_node(
    database: &GraphTransaction<'_>,
    artifact: &SemanticArtifact,
) -> Result<()> {
    delete_artifact_edges_by_id(database, &artifact.artifact_id);
    delete_nodes_by_property(
        database,
        "SemanticArtifact",
        "artifact_id",
        &artifact.artifact_id,
    );
    insert_artifact_node(database, artifact).map(|_| ())
}

#[cfg(test)]
pub(crate) fn insert_artifact_node(
    database: &GraphTransaction<'_>,
    artifact: &SemanticArtifact,
) -> Result<NodeId> {
    let artifact_id =
        database.create_node_with_props(&["SemanticArtifact"], artifact_properties(artifact)?)?;
    link_artifact_to_semantic_element(database, artifact, artifact_id)?;
    Ok(artifact_id)
}

pub(super) fn artifact_properties(
    artifact: &SemanticArtifact,
) -> Result<Vec<(&'static str, GrafeoValue)>> {
    let mut properties = base_artifact_properties(artifact)?;
    properties.extend(storage_alias_properties(&artifact.metadata));
    Ok(properties)
}

pub(super) fn base_artifact_properties(
    artifact: &SemanticArtifact,
) -> Result<Vec<(&'static str, GrafeoValue)>> {
    Ok(vec![
        (
            "artifact_id",
            GrafeoValue::from(artifact.artifact_id.clone()),
        ),
        (
            "semantic_element_id",
            GrafeoValue::from(artifact.semantic_element_id.clone()),
        ),
        (
            "artifact_kind",
            GrafeoValue::from(artifact.artifact_kind.clone()),
        ),
        ("title", GrafeoValue::from(artifact.title.clone())),
        (
            "content_ref",
            GrafeoValue::from(artifact.content_ref.clone().unwrap_or_default()),
        ),
        (
            "content",
            GrafeoValue::from(artifact.content.clone().unwrap_or_default()),
        ),
        (
            "searchable_text",
            GrafeoValue::from(artifact.searchable_text.clone().unwrap_or_default()),
        ),
        ("content_size_bytes", artifact_content_size(artifact)),
        (
            "metadata_json",
            GrafeoValue::from(serde_json::to_string(&artifact.metadata)?),
        ),
        (
            "dependencies_json",
            GrafeoValue::from(serde_json::to_string(&artifact.dependencies)?),
        ),
    ])
}

pub(super) fn artifact_content_size(artifact: &SemanticArtifact) -> GrafeoValue {
    GrafeoValue::from(
        artifact
            .content_size_bytes
            .map(|value| value as i64)
            .unwrap_or(-1),
    )
}

#[cfg(test)]
pub(crate) fn upsert_artifact_vector_node(
    database: &GraphTransaction<'_>,
    artifact: &SemanticArtifact,
    source_text: &str,
    vector: &ArtifactTextVector,
) -> Result<()> {
    delete_nodes_by_property(
        database,
        "SemanticArtifactVector",
        "artifact_id",
        &artifact.artifact_id,
    );
    database.create_node_with_props(
        &["SemanticArtifactVector"],
        artifact_vector_props(artifact, source_text, vector)?,
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn upsert_element_name_vector_node(
    database: &GraphTransaction<'_>,
    element: &SemanticElement,
    source_text: &str,
    vector: &ArtifactTextVector,
) -> Result<()> {
    delete_nodes_by_property(
        database,
        "SemanticElementNameVector",
        SEMANTIC_ELEMENT_ID_PROPERTY,
        &element.semantic_element_id,
    );
    database.create_node_with_props(
        &["SemanticElementNameVector"],
        element_name_vector_props(element, source_text, vector)?,
    )?;
    Ok(())
}
#[cfg(test)]
pub(super) fn link_artifact_to_semantic_element(
    database: &GraphTransaction<'_>,
    artifact: &SemanticArtifact,
    artifact_node_id: NodeId,
) -> Result<()> {
    let Some(element_node_id) =
        transaction_semantic_element_node_id(database, &artifact.semantic_element_id)
    else {
        return Ok(());
    };
    database.create_edge_with_props(
        element_node_id,
        artifact_node_id,
        SEMANTIC_ARTIFACT_EDGE_TYPE,
        [
            (
                SEMANTIC_ELEMENT_ID_PROPERTY,
                GrafeoValue::from(artifact.semantic_element_id.clone()),
            ),
            (
                ARTIFACT_ID_PROPERTY,
                GrafeoValue::from(artifact.artifact_id.clone()),
            ),
        ],
    )?;
    Ok(())
}
pub(crate) fn artifact_dependents(
    database: &GrafeoDB,
    target_kind: &str,
    target_id: &str,
) -> Vec<SemanticArtifact> {
    let (label, property) = match target_kind {
        "semantic_element" => ("SemanticElement", SEMANTIC_ELEMENT_ID_PROPERTY),
        "artifact" => ("SemanticArtifact", ARTIFACT_ID_PROPERTY),
        _ => return Vec::new(),
    };
    let targets = node_ids_by_label_and_property(database, label, property, target_id);
    let mut artifacts = targets
        .iter()
        .flat_map(|node_id| database.store().edges_from(*node_id, Direction::Incoming))
        .filter_map(|(_, edge_id)| database.get_edge(edge_id))
        .filter(|edge| edge.edge_type == ARTIFACT_DEPENDENCY_EDGE_TYPE)
        .filter(|edge| {
            edge_string_property(edge, ARTIFACT_DEPENDENCY_TARGET_KIND_PROPERTY).as_deref()
                == Some(target_kind)
                && edge_string_property(edge, ARTIFACT_DEPENDENCY_TARGET_ID_PROPERTY).as_deref()
                    == Some(target_id)
        })
        .filter_map(|edge| database.get_node(edge.src))
        .filter(|node| node.has_label("SemanticArtifact"))
        .filter_map(|node| semantic_artifact_from_node(&node))
        .collect::<Vec<_>>();
    artifacts.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
    artifacts.dedup_by(|left, right| left.artifact_id == right.artifact_id);
    artifacts
}

#[cfg(test)]
pub(crate) fn delete_artifact_edges_by_id(
    database: &GraphTransaction<'_>,
    artifact_id: &str,
) -> usize {
    let edge_ids = database
        .iter_edges()
        .filter(|edge| edge.edge_type == SEMANTIC_ARTIFACT_EDGE_TYPE)
        .filter(|edge| {
            edge_string_property(edge, ARTIFACT_ID_PROPERTY).as_deref() == Some(artifact_id)
        })
        .map(|edge| edge.id)
        .collect::<Vec<_>>();
    let count = edge_ids.len();
    for edge_id in edge_ids {
        database.delete_edge(edge_id);
    }
    count
}
