//! One scoped selection owner; sources may fetch records selectively.
mod dependency;
use crate::{
    DbError, ProjectSnapshotScope, Result, ScopedSemanticRead, SemanticDependencyDirection,
    SemanticElement, SemanticRelationship, SemanticScopedGraph,
};
use std::collections::{BTreeMap, HashSet, VecDeque};
pub(crate) trait GraphViewSource {
    fn validate_root(&self, root: &str) -> Result<()>;
    fn element(&self, id: &str) -> Option<SemanticElement>;
    fn elements(&self, root: &str, ids: &HashSet<String>, inactive: bool) -> Vec<SemanticElement>;
    fn roots(&self, root: &str, inactive: bool) -> Vec<String>;
    fn edges(
        &self,
        root: &str,
        id: &str,
        direction: SemanticDependencyDirection,
    ) -> Vec<SemanticRelationship>;
    fn location(
        &self,
        root: &str,
        path: Option<&str>,
        line: i64,
        inactive: bool,
    ) -> Vec<SemanticElement>;
    fn scope_count(&self, root: &str, inactive: bool) -> usize;
}
pub(crate) fn scoped_from_source(
    source: &dyn GraphViewSource,
    request: &ScopedSemanticRead,
    revision: i64,
    published_at: String,
) -> Result<SemanticScopedGraph> {
    let (project_root, elements, relationships, scope_element_count) =
        select_records(source, request)?;
    Ok(SemanticScopedGraph {
        commit_version: revision,
        published_at,
        project_root,
        elements,
        relationships,
        scope_element_count,
    })
}
type ScopedRecords = (
    String,
    Vec<SemanticElement>,
    Vec<SemanticRelationship>,
    Option<usize>,
);
fn select_records(
    source: &dyn GraphViewSource,
    request: &ScopedSemanticRead,
) -> Result<ScopedRecords> {
    match request {
        ScopedSemanticRead::Dependency {
            semantic_element_id,
            direction,
            max_depth,
            include_descendants,
            include_inactive,
        } => dependency::select_dependency_records(
            source,
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
            let (root, roots) = structure_roots(source, scope, *include_inactive)?;
            let (elements, edges) = StructureSelection::new(source, &root, *include_inactive)
                .select(roots, *max_depth)?;
            Ok((root, elements, edges, None))
        }
        ScopedSemanticRead::Location {
            project_root,
            path,
            line,
            include_inactive,
        } => {
            source.validate_root(project_root)?;
            if *line < 1 {
                return Err(DbError::invalid_value(line.to_string(), "line >= 1"));
            }
            let mut elements =
                source.location(project_root, path.as_deref(), *line, *include_inactive);
            elements
                .sort_by(|left, right| left.semantic_element_id.cmp(&right.semantic_element_id));
            let edges = location_ancestors(source, project_root, &elements);
            Ok((project_root.clone(), elements, edges, None))
        }
    }
}
fn location_ancestors(
    graph: &dyn GraphViewSource,
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
        for edge in structural_edges(
            graph,
            project_root,
            &id,
            SemanticDependencyDirection::Dependents,
        ) {
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
    graph: &dyn GraphViewSource,
    scope: &ProjectSnapshotScope,
    include_inactive: bool,
) -> Result<(String, Vec<String>)> {
    match scope {
        ProjectSnapshotScope::ProjectRoot(root) => {
            graph.validate_root(root)?;
            Ok((root.clone(), graph.roots(root, include_inactive)))
        }
        ProjectSnapshotScope::SemanticElement(id) => {
            let element = graph
                .element(id)
                .ok_or_else(|| DbError::invalid_value(id, "existing semantic element"))?;
            Ok((element.project_root, vec![id.clone()]))
        }
    }
}

struct StructureSelection<'graph> {
    graph: &'graph dyn GraphViewSource,
    project_root: &'graph str,
    include_inactive: bool,
    elements: BTreeMap<String, SemanticElement>,
    relationships: BTreeMap<(String, String, String, String), SemanticRelationship>,
}

impl<'graph> StructureSelection<'graph> {
    fn new(
        graph: &'graph dyn GraphViewSource,
        project_root: &'graph str,
        include_inactive: bool,
    ) -> Self {
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
        let Some(element) = self.graph.elements(self.project_root, &ids, true).pop() else {
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
            SemanticDependencyDirection::Both
        } else {
            SemanticDependencyDirection::Dependents
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
    graph: &dyn GraphViewSource,
    project_root: &str,
    id: &str,
    direction: SemanticDependencyDirection,
) -> Vec<SemanticRelationship> {
    semantic_edges(graph, project_root, id, direction)
        .into_iter()
        .filter(|edge| edge.relationship_kind == "contains" || edge.label == "contains")
        .collect()
}

fn semantic_edges(
    graph: &dyn GraphViewSource,
    project_root: &str,
    id: &str,
    direction: SemanticDependencyDirection,
) -> Vec<SemanticRelationship> {
    graph.edges(project_root, id, direction)
}
