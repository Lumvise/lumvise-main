//! Replaces native working-artifact properties and dependency edges from an accepted policy plan.
use super::*;

pub(super) struct WorkingArtifactProjection {
    record: SemanticRecordRef,
    artifact: SemanticArtifact,
    properties: Vec<(&'static str, GrafeoValue)>,
    dependency_edges: Vec<EdgeId>,
    vectors: Vec<NodeId>,
}

pub(super) fn prepare(
    database: &GrafeoDB,
    state: &SemanticRevisionState,
    changes: &RevisionProjectionChanges<'_>,
) -> Result<Vec<WorkingArtifactProjection>> {
    let previous = state
        .records
        .iter()
        .map(|row| (&row.record, &row.body))
        .collect::<BTreeMap<_, _>>();
    changes
        .records
        .iter()
        .filter(|row| {
            previous
                .get(&row.record)
                .is_some_and(|body| **body != row.body)
        })
        .filter_map(|row| match &row.body {
            SemanticRevisionBody::Artifact(artifact) => {
                Some(prepare_artifact(database, &state.project_root, artifact))
            }
            _ => None,
        })
        .collect()
}

fn prepare_artifact(
    database: &GrafeoDB,
    root: &str,
    artifact: &SemanticArtifact,
) -> Result<WorkingArtifactProjection> {
    let record = SemanticRecordRef::artifact(&artifact.artifact_id);
    let mut dependency_edges = Vec::new();
    if let Some(node) = existing_projection_node(database, &record, root)? {
        // GRAFEO06-REVIEW(16): planning read of committed adjacency; 0.5 store().edges_from was live.
        for (_, edge) in database
            .get_node_edges(node, Direction::Outgoing, &[])
            .db()?
        {
            if let Some(edge) = database.get_edge(edge).db()?
                && edge.edge_type == ARTIFACT_DEPENDENCY_EDGE_TYPE
            {
                dependency_edges.push(edge.id);
            }
        }
    }
    Ok(WorkingArtifactProjection {
        record,
        artifact: artifact.clone(),
        properties: artifact_properties(artifact)?,
        dependency_edges,
        vectors: node_ids_by_label_and_property(
            database,
            "SemanticArtifactVector",
            ARTIFACT_ID_PROPERTY,
            &artifact.artifact_id,
        )?,
    })
}

impl WorkingArtifactProjection {
    pub(super) fn dependencies(&self) -> Vec<SemanticRecordRef> {
        self.artifact.revision_dependencies()
    }

    pub(super) fn apply(
        self,
        graph: &GraphTransaction<'_>,
        nodes: &RevisionNodeIndex,
    ) -> Result<()> {
        let source = required_node(nodes, &self.record)?;
        for edge in self.dependency_edges {
            graph.delete_edge(edge)?;
        }
        for vector in self.vectors {
            graph.delete_node(vector)?;
        }
        for (name, value) in self.properties {
            graph.set_node_property(source, name, value)?;
        }
        for (dependency, reference) in self
            .artifact
            .dependencies
            .iter()
            .zip(self.artifact.revision_dependencies())
        {
            create_artifact_dependency_edge(
                graph,
                source,
                required_node(nodes, &reference)?,
                dependency,
            )?;
        }
        Ok(())
    }
}
