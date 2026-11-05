//! Private repair of duplicate storage nodes during authoritative semantic sync.
//! The sync owner prepares this plan and applies it after relationship changes.

use super::*;

pub(crate) struct DuplicateElementRepair {
    canonical_nodes: BTreeMap<NodeId, NodeId>,
    incident_edge_ids: BTreeSet<EdgeId>,
}

impl DuplicateElementRepair {
    pub(crate) fn prepare(database: &GrafeoDB, plans: &[ElementUpsertPlan]) -> Result<Self> {
        let canonical_nodes = plans
            .iter()
            .flat_map(|plan| {
                let retained = plan.existing_node_ids.first().copied();
                plan.existing_node_ids
                    .iter()
                    .skip(1)
                    .filter_map(move |duplicate| retained.map(|node| (*duplicate, node)))
            })
            .collect::<BTreeMap<_, _>>();
        let mut incident_edge_ids = BTreeSet::new();
        for node in canonical_nodes.keys() {
            // GRAFEO06-REVIEW(14): planning read of committed adjacency; 0.5 store().edges_from was live.
            for (_, edge_id) in database.get_node_edges(*node, Direction::Both, &[]).db()? {
                incident_edge_ids.insert(edge_id);
            }
        }
        Ok(Self {
            canonical_nodes,
            incident_edge_ids,
        })
    }

    pub(crate) fn apply(self, graph: &GraphTransaction<'_>) -> Result<()> {
        for edge_id in &self.incident_edge_ids {
            self.rebind_edge(graph, *edge_id)?;
        }
        for duplicate in self.canonical_nodes.keys() {
            graph.delete_node(*duplicate)?;
        }
        Ok(())
    }

    fn rebind_edge(&self, graph: &GraphTransaction<'_>, edge_id: EdgeId) -> Result<()> {
        // Read transaction state so deleted relationships stay deleted and updates survive.
        let Some(edge) = graph.get_edge(edge_id)? else {
            return Ok(());
        };
        let source = self
            .canonical_nodes
            .get(&edge.src)
            .copied()
            .unwrap_or(edge.src);
        let target = self
            .canonical_nodes
            .get(&edge.dst)
            .copied()
            .unwrap_or(edge.dst);
        graph.create_edge_with_props(
            source,
            target,
            &edge.edge_type,
            edge.properties
                .iter()
                .map(|(key, value)| (key.as_str(), value.clone())),
        )?;
        graph.delete_edge(edge_id)?;
        Ok(())
    }
}
