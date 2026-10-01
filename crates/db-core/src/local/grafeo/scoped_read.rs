//! Owns scoped graph selection; callers use SemanticPersistence::ScopedRead.

mod dependency;
mod roots;

use std::collections::{BTreeMap, HashSet, VecDeque};

use grafeo::{GrafeoDB, Value};
use grafeo_core::graph::Direction;

use super::graph_row_projection::semantic_relationship_from_edge;
use super::graph_rows::{elements_by_ids_selective, semantic_element_by_id_selective};
use super::semantic_storage::SemanticStorage;
use crate::{
    DbError, ProjectSnapshotScope, Result, ScopedSemanticRead, SemanticElement,
    SemanticRelationship, SemanticScopedGraph,
};

impl SemanticStorage<'_> {
    pub(crate) fn scoped_read(&self, request: &ScopedSemanticRead) -> Result<SemanticScopedGraph> {
        self.graph.stable_read(|graph| {
            let (commit_version, published_at) = self.latest_graph_publication()?;
            let (project_root, elements, relationships, scope_element_count) =
                self.select_scoped_records(graph, request)?;
            Ok(SemanticScopedGraph {
                commit_version,
                published_at,
                project_root,
                elements,
                relationships,
                scope_element_count,
            })
        })
    }
}

impl SemanticStorage<'_> {
    fn select_scoped_records(
        &self,
        graph: &GrafeoDB,
        request: &ScopedSemanticRead,
    ) -> Result<(
        String,
        Vec<SemanticElement>,
        Vec<SemanticRelationship>,
        Option<usize>,
    )> {
        match request {
            ScopedSemanticRead::Dependency {
                semantic_element_id,
                direction,
                max_depth,
                include_descendants,
                include_inactive,
            } => dependency::select_dependency_records(
                self.graph,
                graph,
                semantic_element_id,
                *direction,
                *max_depth,
                *include_descendants,
                *include_inactive,
            ),
            ScopedSemanticRead::Structure {
                scope,
                max_depth,
                include_inactive,
            } => {
                let (root, roots) = structure_roots(graph, scope, *include_inactive)?;
                let (elements, edges) = StructureSelection::new(graph, &root, *include_inactive)
                    .select(roots, *max_depth)?;
                Ok((root, elements, edges, None))
            }
            ScopedSemanticRead::Location {
                project_root,
                path,
                line,
                include_inactive,
            } => {
                crate::local::sql::validation::require_non_empty(
                    project_root,
                    "non-empty project root",
                )?;
                if *line < 1 {
                    return Err(DbError::invalid_value(line.to_string(), "line >= 1"));
                }
                let mut elements = self.graph.location_elements(
                    graph,
                    project_root,
                    path.as_deref(),
                    *line,
                    *include_inactive,
                );
                elements.sort_by(|a, b| a.semantic_element_id.cmp(&b.semantic_element_id));
                let edges = location_ancestors(graph, project_root, &elements);
                Ok((project_root.clone(), elements, edges, None))
            }
        }
    }
}

fn location_ancestors(
    graph: &GrafeoDB,
    project_root: &str,
    elements: &[SemanticElement],
) -> Vec<SemanticRelationship> {
    let mut pending = elements
        .iter()
        .map(|element| element.semantic_element_id.clone())
        .collect::<Vec<_>>();
    let mut visited = HashSet::new();
    let mut edges = BTreeMap::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        for edge in structural_edges(graph, project_root, &id, Direction::Incoming) {
            pending.push(edge.source_element_id.clone());
            edges.insert(structural_identity(&edge), edge);
        }
    }
    edges.into_values().collect()
}

fn structural_identity(edge: &SemanticRelationship) -> (String, String, String, String) {
    (
        edge.source_element_id.clone(),
        edge.target_element_id.clone(),
        edge.relationship_kind.clone(),
        edge.label.clone(),
    )
}

fn structure_roots(
    graph: &GrafeoDB,
    scope: &ProjectSnapshotScope,
    include_inactive: bool,
) -> Result<(String, Vec<String>)> {
    match scope {
        ProjectSnapshotScope::ProjectRoot(root) => {
            crate::local::sql::validation::require_non_empty(root, "non-empty project root")?;
            Ok((
                root.clone(),
                roots::project_roots(graph, root, include_inactive),
            ))
        }
        ProjectSnapshotScope::SemanticElement(id) => {
            let element = semantic_element_by_id_selective(graph, id)
                .ok_or_else(|| DbError::invalid_value(id, "existing semantic element"))?;
            Ok((element.project_root, vec![id.clone()]))
        }
    }
}

struct StructureSelection<'graph> {
    graph: &'graph GrafeoDB,
    project_root: &'graph str,
    include_inactive: bool,
    elements: BTreeMap<String, SemanticElement>,
    relationships: BTreeMap<(String, String, String, String), SemanticRelationship>,
}

impl<'graph> StructureSelection<'graph> {
    fn new(graph: &'graph GrafeoDB, project_root: &'graph str, include_inactive: bool) -> Self {
        Self {
            graph,
            project_root,
            include_inactive,
            elements: BTreeMap::new(),
            relationships: BTreeMap::new(),
        }
    }

    fn select(
        mut self,
        roots: Vec<String>,
        max_depth: usize,
    ) -> Result<(Vec<SemanticElement>, Vec<SemanticRelationship>)> {
        if max_depth > 128 {
            return Err(DbError::invalid_value(
                max_depth.to_string(),
                "structural max_depth from 0 through 128",
            ));
        }
        let mut pending = roots.into_iter().map(|id| (id, 0)).collect::<VecDeque<_>>();
        let mut visited = HashSet::new();
        while let Some((id, depth)) = pending.pop_front() {
            if !visited.insert(id.clone()) || !self.include_element(&id) {
                continue;
            }
            let children = self.read_edges(&id, depth < max_depth);
            pending.extend(children.into_iter().map(|child| (child, depth + 1)));
        }
        Ok((
            self.elements.into_values().collect(),
            self.relationships.into_values().collect(),
        ))
    }

    fn include_element(&mut self, id: &str) -> bool {
        let ids = HashSet::from([id.to_owned()]);
        let Some(element) =
            elements_by_ids_selective(self.graph, self.project_root, &ids, true).pop()
        else {
            return false;
        };
        if !self.include_inactive && element.lifecycle != "active" {
            return false;
        }
        self.elements.insert(id.to_owned(), element);
        true
    }

    fn read_edges(&mut self, id: &str, include_children: bool) -> Vec<String> {
        let direction = if include_children {
            Direction::Both
        } else {
            Direction::Incoming
        };
        let edges = structural_edges(self.graph, self.project_root, id, direction);
        let mut children = Vec::new();
        for edge in edges {
            if include_children && edge.source_element_id == id {
                children.push(edge.target_element_id.clone());
            }
            let key = structural_identity(&edge);
            self.relationships.insert(key, edge);
        }
        children
    }
}

fn structural_edges(
    graph: &GrafeoDB,
    project_root: &str,
    id: &str,
    direction: Direction,
) -> Vec<SemanticRelationship> {
    semantic_edges(graph, project_root, id, direction)
        .into_iter()
        .filter(|edge| edge.relationship_kind == "contains" || edge.label == "contains")
        .collect()
}

fn semantic_edges(
    graph: &GrafeoDB,
    project_root: &str,
    id: &str,
    direction: Direction,
) -> Vec<SemanticRelationship> {
    graph
        .find_nodes_by_property("semantic_element_id", &Value::from(id))
        .into_iter()
        .flat_map(|node| {
            graph
                .store()
                .edges_from(node, direction)
                .collect::<Vec<_>>()
        })
        .filter_map(|(_, edge)| graph.get_edge(edge))
        .filter_map(|edge| semantic_relationship_from_edge(&edge))
        .filter(|edge| edge.project_root == project_root)
        .collect()
}
