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
                    && element
                        .start_line
                        .filter(|value| *value >= 0)
                        .is_none_or(|start| start <= line)
                    && element
                        .end_line
                        .filter(|value| *value >= 0)
                        .is_none_or(|end| end >= line)
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
fn normalized_path(path: &str) -> &str {
    path.trim().trim_start_matches("./").trim_start_matches('/')
}
