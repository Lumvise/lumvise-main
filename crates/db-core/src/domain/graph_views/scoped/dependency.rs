//! Selects the records consumed by dependency presentation, under its parent's
//! stable graph lease. Traversal never loads unrelated record bodies or artifacts.

use super::*;
use crate::SemanticDependencyDirection;
use std::collections::HashMap;

pub(super) fn select_dependency_records(
    graph: &dyn GraphViewSource,
    root_id: &str,
    direction: SemanticDependencyDirection,
    max_depth: usize,
    include_descendants: bool,
    include_inactive: bool,
) -> Result<(
    String,
    Vec<SemanticElement>,
    Vec<SemanticRelationship>,
    Option<usize>,
)> {
    if max_depth > 128 {
        return Err(DbError::invalid_value(
            max_depth.to_string(),
            "dependency max_depth from 0 through 128",
        ));
    }
    let root = graph
        .element(root_id)
        .filter(|element| include_inactive || element.lifecycle == "active")
        .ok_or_else(|| DbError::invalid_value(root_id, "existing semantic element"))?;
    let count = if include_descendants {
        graph.scope_count(&root.project_root, include_inactive)
    } else {
        1
    };
    let mut selection =
        DependencySelection::new(graph, &root, include_descendants, include_inactive);
    selection.visit(root_id, direction, max_depth);
    Ok((
        root.project_root,
        selection.elements.into_values().collect(),
        selection.relationships.into_values().collect(),
        Some(count),
    ))
}

struct DependencySelection<'graph> {
    graph: &'graph dyn GraphViewSource,
    project_root: String,
    include_descendants: bool,
    include_inactive: bool,
    elements: BTreeMap<String, SemanticElement>,
    relationships: BTreeMap<(String, String, String, String), SemanticRelationship>,
}

impl<'graph> DependencySelection<'graph> {
    fn new(
        graph: &'graph dyn GraphViewSource,
        root: &SemanticElement,
        include_descendants: bool,
        include_inactive: bool,
    ) -> Self {
        Self {
            graph,
            project_root: root.project_root.clone(),
            include_descendants,
            include_inactive,
            elements: BTreeMap::new(),
            relationships: BTreeMap::new(),
        }
    }

    fn visit(&mut self, root_id: &str, direction: SemanticDependencyDirection, max_depth: usize) {
        let mut pending = VecDeque::from([(root_id.to_owned(), 0)]);
        let mut visited = HashMap::<String, usize>::new();
        while let Some((id, depth)) = pending.pop_front() {
            if visited.get(&id).is_some_and(|previous| *previous <= depth)
                || !self.include_element(&id)
            {
                continue;
            }
            visited.insert(id.clone(), depth);
            if depth >= max_depth {
                continue;
            }
            let direction = if depth == 0 {
                direction
            } else {
                SemanticDependencyDirection::Both
            };
            let neighbors = self.select_neighbors(&id, direction);
            pending.extend(neighbors.into_iter().map(|next| (next, depth + 1)));
        }
    }

    fn include_element(&mut self, id: &str) -> bool {
        let Some(element) = self.graph.element(id).filter(|element| {
            element.project_root == self.project_root
                && (self.include_inactive || element.lifecycle == "active")
        }) else {
            return false;
        };
        self.elements.insert(id.to_owned(), element);
        true
    }

    fn select_neighbors(
        &mut self,
        id: &str,
        direction: SemanticDependencyDirection,
    ) -> HashSet<String> {
        let scope = self.descendant_scope(id);
        let mut neighbors = HashSet::new();
        let graph_direction = match direction {
            SemanticDependencyDirection::Dependencies => SemanticDependencyDirection::Dependencies,
            SemanticDependencyDirection::Dependents => SemanticDependencyDirection::Dependents,
            SemanticDependencyDirection::Both => SemanticDependencyDirection::Both,
        };
        for id in scope {
            for edge in semantic_edges(self.graph, &self.project_root, &id, graph_direction) {
                if direction != SemanticDependencyDirection::Dependents
                    && edge.source_element_id == id
                {
                    neighbors.insert(edge.target_element_id.clone());
                }
                if direction != SemanticDependencyDirection::Dependencies
                    && edge.target_element_id == id
                {
                    neighbors.insert(edge.source_element_id.clone());
                }
                self.relationships.insert(structural_identity(&edge), edge);
            }
        }
        neighbors
    }

    fn descendant_scope(&mut self, id: &str) -> HashSet<String> {
        let mut ids = HashSet::from([id.to_owned()]);
        if !self.include_descendants {
            return ids;
        }
        let mut pending = vec![id.to_owned()];
        while let Some(parent) = pending.pop() {
            for edge in structural_edges(
                self.graph,
                &self.project_root,
                &parent,
                SemanticDependencyDirection::Dependencies,
            ) {
                if ids.insert(edge.target_element_id.clone()) {
                    pending.push(edge.target_element_id.clone());
                }
                self.relationships.insert(structural_identity(&edge), edge);
            }
        }
        ids
    }
}
