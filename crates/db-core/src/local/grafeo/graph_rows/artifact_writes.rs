use super::*;

pub(crate) fn prepare_artifact_upsert(
    database: &GrafeoDB,
    artifact: &SemanticArtifact,
    vector: Option<(&str, &ArtifactTextVector)>,
) -> Result<ArtifactUpsertPlan> {
    let mut owner_node_ids = Vec::new();
    for node_id in node_ids_by_label_and_property(
        database,
        "SemanticElement",
        SEMANTIC_ELEMENT_ID_PROPERTY,
        &artifact.semantic_element_id,
    )? {
        if database
            .get_node(node_id)
            .db()?
            .is_some_and(|node| element_node_is_active(&node))
        {
            owner_node_ids.push(node_id);
        }
    }
    let [owner_node_id] = owner_node_ids.as_slice() else {
        return Err(DbError::invalid_value(
            &artifact.semantic_element_id,
            "exactly one existing semantic element for artifact",
        ));
    };
    let owner_element = database
        .get_node(*owner_node_id)
        .db()?
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
            dependency_target_node(database, dependency)?
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
    )?;
    // Artifact updates must scale with their own links, not the whole project's graph.
    // GRAFEO06-REVIEW(10): planning reads of committed adjacency; 0.5 store().edges_from was live.
    let mut dependency_edge_ids = Vec::new();
    let mut inbound_dependency_rebinds = Vec::new();
    let mut artifact_edge_ids = Vec::new();
    for node_id in &artifact_node_ids {
        for (_, edge_id) in database
            .get_node_edges(*node_id, Direction::Outgoing, &[])
            .db()?
        {
            if let Some(edge) = database.get_edge(edge_id).db()?
                && edge.edge_type == ARTIFACT_DEPENDENCY_EDGE_TYPE
            {
                dependency_edge_ids.push(edge.id);
            }
        }
    }
    for node_id in &artifact_node_ids {
        for (_, edge_id) in database
            .get_node_edges(*node_id, Direction::Incoming, &[])
            .db()?
        {
            let Some(edge) = database.get_edge(edge_id).db()? else {
                continue;
            };
            if edge.edge_type == ARTIFACT_DEPENDENCY_EDGE_TYPE
                && let Some(target_kind) =
                    edge_string_property(&edge, ARTIFACT_DEPENDENCY_TARGET_KIND_PROPERTY)
                && let Some(target_id) =
                    edge_string_property(&edge, ARTIFACT_DEPENDENCY_TARGET_ID_PROPERTY)
            {
                inbound_dependency_rebinds.push(ArtifactDependencyRebind {
                    source_node_id: edge.src,
                    target_kind,
                    target_id,
                });
            }
        }
    }
    for node_id in &artifact_node_ids {
        for (_, edge_id) in database
            .get_node_edges(*node_id, Direction::Incoming, &[])
            .db()?
        {
            if let Some(edge) = database.get_edge(edge_id).db()?
                && edge.edge_type == SEMANTIC_ARTIFACT_EDGE_TYPE
                && edge_string_property(&edge, ARTIFACT_ID_PROPERTY).as_deref()
                    == Some(&artifact.artifact_id)
            {
                artifact_edge_ids.push(edge.id);
            }
        }
    }
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
        )?,
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
        database.delete_edge(edge_id)?;
    }
    for edge_id in plan.dependency_edge_ids {
        database.delete_edge(edge_id)?;
    }
    for node_id in plan.artifact_node_ids {
        database.delete_node(node_id)?;
    }
    for node_id in plan.vector_node_ids {
        database.delete_node(node_id)?;
    }
    let artifact_node_id =
        database.create_node_with_props(&["SemanticArtifact"], plan.artifact_properties)?;
    create_artifact_owner_edge(
        database,
        plan.owner_node_id,
        artifact_node_id,
        &artifact.artifact_id,
        &artifact.semantic_element_id,
    )?;
    for (dependency, target_node_id) in plan.dependency_targets {
        create_artifact_dependency_edge(database, artifact_node_id, target_node_id, &dependency)?;
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
    delete_artifact_edges_by_id(database, &artifact.artifact_id)?;
    delete_nodes_by_property(
        database,
        "SemanticArtifact",
        "artifact_id",
        &artifact.artifact_id,
    )?;
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
    )?;
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
    )?;
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
        transaction_semantic_element_node_id(database, &artifact.semantic_element_id)?
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
) -> Result<Vec<SemanticArtifact>> {
    let (label, property) = match target_kind {
        "semantic_element" => ("SemanticElement", SEMANTIC_ELEMENT_ID_PROPERTY),
        "artifact" => ("SemanticArtifact", ARTIFACT_ID_PROPERTY),
        _ => return Ok(Vec::new()),
    };
    let targets = node_ids_by_label_and_property(database, label, property, target_id)?;
    let mut artifacts = Vec::new();
    for node_id in &targets {
        // GRAFEO06-REVIEW(11): committed incoming adjacency; 0.5 store().edges_from was live.
        for (_, edge_id) in database
            .get_node_edges(*node_id, Direction::Incoming, &[])
            .db()?
        {
            let Some(edge) = database.get_edge(edge_id).db()? else {
                continue;
            };
            if edge.edge_type == ARTIFACT_DEPENDENCY_EDGE_TYPE
                && edge_string_property(&edge, ARTIFACT_DEPENDENCY_TARGET_KIND_PROPERTY).as_deref()
                    == Some(target_kind)
                && edge_string_property(&edge, ARTIFACT_DEPENDENCY_TARGET_ID_PROPERTY).as_deref()
                    == Some(target_id)
                && let Some(node) = database.get_node(edge.src).db()?
                && node.has_label("SemanticArtifact")
                && let Some(artifact) = semantic_artifact_from_node(&node)
            {
                artifacts.push(artifact);
            }
        }
    }
    artifacts.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
    artifacts.dedup_by(|left, right| left.artifact_id == right.artifact_id);
    Ok(artifacts)
}

#[cfg(test)]
pub(crate) fn delete_artifact_edges_by_id(
    database: &GraphTransaction<'_>,
    artifact_id: &str,
) -> Result<usize> {
    let edge_ids = database
        .iter_edges()
        .into_iter()
        .filter(|edge| edge.edge_type == SEMANTIC_ARTIFACT_EDGE_TYPE)
        .filter(|edge| {
            edge_string_property(edge, ARTIFACT_ID_PROPERTY).as_deref() == Some(artifact_id)
        })
        .map(|edge| edge.id)
        .collect::<Vec<_>>();
    let count = edge_ids.len();
    for edge_id in edge_ids {
        database.delete_edge(edge_id)?;
    }
    Ok(count)
}

/// Physical owner projection shared by current artifact writes and immutable revision publication.
pub(crate) fn create_artifact_owner_edge(
    database: &GraphTransaction<'_>,
    owner: NodeId,
    artifact: NodeId,
    artifact_id: &str,
    element_id: &str,
) -> Result<()> {
    database.create_edge_with_props(
        owner,
        artifact,
        SEMANTIC_ARTIFACT_EDGE_TYPE,
        [
            (SEMANTIC_ELEMENT_ID_PROPERTY, GrafeoValue::from(element_id)),
            (ARTIFACT_ID_PROPERTY, GrafeoValue::from(artifact_id)),
        ],
    )?;
    Ok(())
}

/// Exact dependency endpoints are never rebound to a newer record during revision publication.
pub(crate) fn create_artifact_dependency_edge(
    database: &GraphTransaction<'_>,
    artifact: NodeId,
    target: NodeId,
    dependency: &ArtifactDependency,
) -> Result<()> {
    let kind = match dependency.target {
        ArtifactDependencyTarget::SemanticElement { .. } => "semantic_element",
        ArtifactDependencyTarget::Artifact { .. } => "artifact",
    };
    database.create_edge_with_props(
        artifact,
        target,
        ARTIFACT_DEPENDENCY_EDGE_TYPE,
        [
            (
                ARTIFACT_DEPENDENCY_TARGET_KIND_PROPERTY,
                GrafeoValue::from(kind),
            ),
            (
                ARTIFACT_DEPENDENCY_TARGET_ID_PROPERTY,
                GrafeoValue::from(dependency.target_id()),
            ),
        ],
    )?;
    Ok(())
}
