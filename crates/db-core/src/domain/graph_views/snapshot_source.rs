//! In-memory source for portable snapshots; local sources retain selective I/O.
use super::*;
use crate::SemanticDependencyDirection;
impl GraphViewSource for SemanticProjectSnapshot {
    fn validate_root(&self, root: &str) -> Result<()> {
        super::validate_root(&self.project_root, root)
    }
    fn element(&self, id: &str) -> Option<SemanticElement> {
        self.elements
            .iter()
            .find(|element| element.semantic_element_id == id && element.lifecycle != "inactive")
            .cloned()
    }
    fn elements(&self, root: &str, ids: &HashSet<String>, inactive: bool) -> Vec<SemanticElement> {
        self.elements
            .iter()
            .filter(|element| {
                element.project_root == root
                    && ids.contains(&element.semantic_element_id)
                    && (inactive || element.lifecycle != "inactive")
            })
            .cloned()
            .collect()
    }
    fn roots(&self, root: &str, inactive: bool) -> Vec<String> {
        let members = self
            .elements
            .iter()
            .filter(|element| {
                element.project_root == root && (inactive || element.lifecycle == "active")
            })
            .map(|element| &element.semantic_element_id)
            .collect::<HashSet<_>>();
        let parented = self
            .relationships
            .iter()
            .filter(|edge| {
                edge.project_root == root
                    && members.contains(&edge.source_element_id)
                    && (edge.relationship_kind == "contains" || edge.label == "contains")
            })
            .map(|edge| &edge.target_element_id)
            .collect::<HashSet<_>>();
        members
            .difference(&parented)
            .map(|id| (*id).clone())
            .collect()
    }
    fn edges(
        &self,
        root: &str,
        id: &str,
        direction: SemanticDependencyDirection,
    ) -> Vec<SemanticRelationship> {
        self.relationships
            .iter()
            .filter(|edge| {
                edge.project_root == root
                    && ((direction != SemanticDependencyDirection::Dependents
                        && edge.source_element_id == id)
                        || (direction != SemanticDependencyDirection::Dependencies
                            && edge.target_element_id == id))
            })
            .cloned()
            .collect()
    }
    fn location(
        &self,
        root: &str,
        path: Option<&str>,
        line: i64,
        inactive: bool,
    ) -> Vec<SemanticElement> {
        self.elements
            .iter()
            .filter(|element| {
                element.project_root == root
                    && path.is_some_and(|path| normalized_path(&element.path) == path)
                    && (inactive || element.lifecycle == "active")
                    && element_contains_line(element, line)
            })
            .cloned()
            .collect()
    }
    fn path_prefix(&self, root: &str, prefix: &str, inactive: bool) -> Vec<SemanticElement> {
        self.elements
            .iter()
            .filter(|element| {
                element.project_root == root
                    && element.path.starts_with(prefix)
                    && (inactive || element.lifecycle == "active")
            })
            .cloned()
            .collect()
    }
    fn scope_count(&self, root: &str, inactive: bool) -> usize {
        self.elements
            .iter()
            .filter(|element| {
                element.project_root == root && (inactive || element.lifecycle == "active")
            })
            .count()
    }
}
pub(crate) fn normalized_path(path: &str) -> &str {
    path.trim().trim_start_matches("./").trim_start_matches('/')
}

pub(crate) fn element_contains_line(element: &SemanticElement, line: i64) -> bool {
    element
        .start_line
        .filter(|value| *value >= 0)
        .is_none_or(|start| start <= line)
        && element
            .end_line
            .filter(|value| *value >= 0)
            .is_none_or(|end| end >= line)
}

/// Per-read indexes keep complete subtree traversal linear in graph size.
/// Selection rules remain in `scoped`; this component only supplies records.
pub(super) struct SnapshotGraphIndex<'a> {
    snapshot: &'a SemanticProjectSnapshot,
    elements: HashMap<&'a str, &'a SemanticElement>,
    outgoing: HashMap<&'a str, Vec<&'a SemanticRelationship>>,
    incoming: HashMap<&'a str, Vec<&'a SemanticRelationship>>,
}
impl<'a> SnapshotGraphIndex<'a> {
    pub(super) fn new(snapshot: &'a SemanticProjectSnapshot) -> Self {
        let elements = snapshot
            .elements
            .iter()
            .map(|element| (element.semantic_element_id.as_str(), element))
            .collect();
        let mut index = Self {
            snapshot,
            elements,
            outgoing: HashMap::new(),
            incoming: HashMap::new(),
        };
        for edge in &snapshot.relationships {
            index
                .outgoing
                .entry(&edge.source_element_id)
                .or_default()
                .push(edge);
            index
                .incoming
                .entry(&edge.target_element_id)
                .or_default()
                .push(edge);
        }
        index
    }
}
impl GraphViewSource for SnapshotGraphIndex<'_> {
    fn validate_root(&self, root: &str) -> Result<()> {
        self.snapshot.validate_root(root)
    }
    fn element(&self, id: &str) -> Option<SemanticElement> {
        self.elements
            .get(id)
            .filter(|element| element.lifecycle != "inactive")
            .map(|element| (*element).clone())
    }
    fn elements(&self, root: &str, ids: &HashSet<String>, inactive: bool) -> Vec<SemanticElement> {
        ids.iter()
            .filter_map(|id| self.elements.get(id.as_str()))
            .filter(|element| {
                element.project_root == root && (inactive || element.lifecycle != "inactive")
            })
            .map(|element| (*element).clone())
            .collect()
    }
    fn roots(&self, root: &str, inactive: bool) -> Vec<String> {
        self.snapshot.roots(root, inactive)
    }
    fn edges(
        &self,
        root: &str,
        id: &str,
        direction: SemanticDependencyDirection,
    ) -> Vec<SemanticRelationship> {
        let outgoing = (direction != SemanticDependencyDirection::Dependents)
            .then(|| self.outgoing.get(id))
            .flatten();
        let incoming = (direction != SemanticDependencyDirection::Dependencies)
            .then(|| self.incoming.get(id))
            .flatten();
        outgoing
            .into_iter()
            .flatten()
            .chain(incoming.into_iter().flatten())
            .filter(|edge| edge.project_root == root)
            .map(|edge| (*edge).clone())
            .collect()
    }
    fn location(
        &self,
        root: &str,
        path: Option<&str>,
        line: i64,
        inactive: bool,
    ) -> Vec<SemanticElement> {
        self.snapshot.location(root, path, line, inactive)
    }
    fn scope_count(&self, root: &str, inactive: bool) -> usize {
        self.snapshot.scope_count(root, inactive)
    }
    fn path_prefix(&self, root: &str, prefix: &str, inactive: bool) -> Vec<SemanticElement> {
        self.snapshot.path_prefix(root, prefix, inactive)
    }
}
