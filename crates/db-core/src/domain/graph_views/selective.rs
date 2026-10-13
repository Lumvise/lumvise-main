//! Complete subtree views compose the same structural traversal as scoped reads.
use super::*;
use crate::SemanticSelectiveSubgraph;

impl SemanticProjectSnapshot {
    /// Returns one active subtree, touching relationships and external endpoints.
    /// Example: `snapshot.selective_subgraph("file:main", Some("knowledge"))?`.
    pub fn selective_subgraph(
        &self,
        root_id: &str,
        namespace: Option<&str>,
    ) -> Result<Option<SemanticSelectiveSubgraph>> {
        if root_id.trim().is_empty() {
            return Err(DbError::invalid_value(root_id, "non-empty root element id"));
        }
        if !self
            .elements
            .iter()
            .any(|element| element.semantic_element_id == root_id && element.lifecycle == "active")
        {
            return Ok(None);
        }
        let source = snapshot_source::SnapshotGraphIndex::new(self);
        let elements = scoped::complete_subtree(&source, &self.project_root, root_id)?;
        let ids = elements
            .iter()
            .map(|element| element.semantic_element_id.as_str())
            .collect::<HashSet<_>>();
        let relationships = self
            .relationships
            .iter()
            .filter(|edge| {
                ids.contains(edge.source_element_id.as_str())
                    || ids.contains(edge.target_element_id.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        let external_ids = relationships
            .iter()
            .flat_map(|edge| [&edge.source_element_id, &edge.target_element_id])
            .collect::<HashSet<_>>();
        let mut external_elements = self
            .elements
            .iter()
            .filter(|element| {
                !ids.contains(element.semantic_element_id.as_str())
                    && external_ids.contains(&element.semantic_element_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        external_elements.sort_by(|a, b| a.semantic_element_id.cmp(&b.semantic_element_id));
        let mut artifacts = self
            .artifacts
            .iter()
            .filter(|artifact| {
                ids.contains(artifact.semantic_element_id.as_str())
                    && namespace.is_none_or(|key| artifact.metadata[key].is_object())
            })
            .cloned()
            .collect::<Vec<_>>();
        artifacts.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
        Ok(Some(SemanticSelectiveSubgraph {
            commit_version: self.commit_version,
            published_at: self.published_at.clone(),
            project_root: self.project_root.clone(),
            root_element_id: root_id.into(),
            elements,
            relationships,
            artifacts,
            external_elements,
        }))
    }
}
