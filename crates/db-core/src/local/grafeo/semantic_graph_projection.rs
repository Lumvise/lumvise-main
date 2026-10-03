use crate::domain::graph_views::{BatchedGraphRows, ProjectionArtifact, ProjectionElement};
use crate::{
    Result, SemanticGraphProjection, SemanticGraphProjectionRequest, SemanticRelationship,
};
use grafeo::{EdgeId, GrafeoDB, NodeId, Value as GrafeoValue};
use grafeo_core::graph::{Direction, GraphStore};
use std::collections::{HashMap, HashSet};
const EDGE_PROPERTIES: &[&str] = &["relationship_kind", "label"];
mod row_decoding;
pub(super) use crate::domain::graph_views::slice_file_projection;
use row_decoding::*;
pub(crate) fn project(
    graph: &GrafeoDB,
    request: &SemanticGraphProjectionRequest,
    commit_version: i64,
    published_at: String,
) -> Result<SemanticGraphProjection> {
    let started = std::time::Instant::now();
    let rows = read_batched_rows(graph, &request.project_root);
    if request.granularity != crate::SemanticGraphGranularity::File {
        metrics::histogram!("lumvise_db_renderer_graph_projection_stage_seconds", "stage" => "batched_rows").record(started.elapsed().as_secs_f64());
    }
    let projection =
        crate::domain::graph_views::project_projection(rows, request, commit_version, published_at);
    if request.granularity == crate::SemanticGraphGranularity::File {
        metrics::histogram!("lumvise_db_renderer_graph_projection_stage_seconds", "stage" => "file_scope").record(started.elapsed().as_secs_f64());
    }
    projection
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::grafeo::graph_rows::{
        insert_relationship_edge_with_index, semantic_node_id_index,
    };
    use crate::local::runtime::DbCore;
    use crate::{SemanticArtifact, SemanticElement, SemanticGraphGranularity};
    use serde_json::json;

    #[test]
    fn batched_projection_preserves_scope_neighbors_aggregation_and_artifacts() {
        let db = DbCore::in_memory().unwrap();
        let storage = db.storage_manager().semantic_storage();
        for element in [
            element("folder", "src", "folder", json!({}), 1, 10),
            element("a", "src/a.rs", "file", json!({}), 2, 4),
            element("b", "src/b.rs", "file", json!({}), 6, 9),
            element("outside", "other.rs", "file", json!({}), 1, 2),
            element(
                "external",
                "vendor.rs",
                "external",
                json!({"external": true}),
                1,
                2,
            ),
        ] {
            storage.upsert_element(&element).unwrap();
        }
        let contains_a = relationship("folder", "a", "contains", "contains");
        let contains_b = relationship("folder", "b", "contains", "contains");
        storage.link_elements(&contains_a).unwrap();
        storage.link_elements(&contains_b).unwrap();
        let link = relationship("a", "b", "references", "references");
        let external_link = relationship("a", "external", "references", "references");
        let outside_link = relationship("b", "outside", "references", "references");
        db.graph
            .write(|graph| {
                let ids = semantic_node_id_index(graph, "/repo");
                insert_relationship_edge_with_index(graph, &link, &ids)?;
                insert_relationship_edge_with_index(graph, &link, &ids)?;
                insert_relationship_edge_with_index(graph, &external_link, &ids)?;
                insert_relationship_edge_with_index(graph, &outside_link, &ids)?;
                let irrelevant_target = graph.create_node_with_props(&["Unrelated"], [])?;
                graph.create_edge_with_props(
                    ids["a"],
                    irrelevant_target,
                    "unrelated",
                    std::iter::empty(),
                )
            })
            .unwrap();

        let content = vec![b'x'; 9_000];
        db.artifact_blobs()
            .put_blob("blob://artifact-a", "artifact-a", "text/plain", &content)
            .unwrap();
        let artifact = SemanticArtifact {
            artifact_id: "artifact-a".into(),
            semantic_element_id: "a".into(),
            artifact_kind: "note".into(),
            title: "A note".into(),
            content_ref: Some("blob://artifact-a".into()),
            content: None,
            searchable_text: None,
            content_size_bytes: Some(content.len()),
            dependencies: vec![],
            metadata: json!({}),
        };
        storage.upsert_artifact(&artifact).unwrap();

        let complete = storage
            .project_renderer_graph(&request(
                None,
                SemanticGraphGranularity::Property,
                false,
                false,
            ))
            .unwrap();
        assert_eq!(
            complete
                .nodes
                .iter()
                .map(|node| node.id.as_str())
                .collect::<Vec<_>>(),
            vec!["outside", "folder", "a", "b"]
        );
        let expected_text = "x".repeat(9_000);
        assert_eq!(
            complete
                .nodes
                .iter()
                .find(|node| node.id == "a")
                .unwrap()
                .artifacts[0]
                .text
                .as_deref(),
            Some(expected_text.as_str())
        );
        assert_eq!(
            complete
                .edges
                .iter()
                .find(|edge| edge.source == "a" && edge.target == "b")
                .unwrap()
                .weight,
            2
        );
        assert_eq!(
            complete
                .edges
                .iter()
                .filter(|edge| edge.relationship_kind == "contains")
                .count(),
            2
        );
        assert!(!complete.nodes.iter().any(|node| node.id == "external"));
        assert!(!complete.edges.iter().any(|edge| edge.target == "external"));

        let first_neighbors = storage
            .project_renderer_graph(&request(
                Some("src/a.rs"),
                SemanticGraphGranularity::File,
                false,
                true,
            ))
            .unwrap();
        assert_eq!(
            first_neighbors
                .nodes
                .iter()
                .map(|node| node.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "external"]
        );

        let with_external = storage
            .project_renderer_graph(&request(
                None,
                SemanticGraphGranularity::Property,
                true,
                false,
            ))
            .unwrap();
        assert!(with_external.nodes.iter().any(|node| node.id == "external"));
        assert!(
            with_external
                .edges
                .iter()
                .any(|edge| edge.source == "a" && edge.target == "external")
        );

        let recursive_files = storage
            .project_renderer_graph(&request(
                Some("src"),
                SemanticGraphGranularity::File,
                false,
                false,
            ))
            .unwrap();
        assert_eq!(
            recursive_files
                .nodes
                .iter()
                .map(|node| node.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );

        let mut empty_request = request(None, SemanticGraphGranularity::Property, true, false);
        empty_request.project_root = "/missing".into();
        let empty = storage.project_renderer_graph(&empty_request).unwrap();
        assert!(empty.nodes.is_empty());
    }

    #[test]
    fn structure_updates_do_not_eagerly_rebuild_renderer_graphs() {
        let db = DbCore::in_memory().unwrap();
        let storage = db.storage_manager().semantic_storage();
        let first = element("a", "src/a.rs", "file", json!({}), 1, 2);
        storage
            .sync_semantic_structure("/repo", &[first], &[])
            .unwrap();
        assert_eq!(db.graph.projection_cache_len(), 0);
        let updated = element("a", "src/a.rs", "file", json!({}), 1, 3);
        storage
            .sync_semantic_partition(
                &crate::SemanticPartition {
                    project_root: "/repo".into(),
                    replace_paths: vec!["src/a.rs".into()],
                },
                &[updated],
                &[],
            )
            .unwrap();
        assert_eq!(db.graph.projection_cache_len(), 0);
        let projection = storage
            .project_renderer_graph(&request(None, SemanticGraphGranularity::File, false, false))
            .unwrap();
        assert_eq!(projection.nodes.len(), 1);
        assert_eq!(projection.nodes[0].id, "a");
    }

    #[test]
    fn file_projection_cache_is_revision_tagged_and_invalidated_by_publication() {
        let db = DbCore::in_memory().unwrap();
        let storage = db.storage_manager().semantic_storage();
        storage
            .upsert_element(&element("a", "src/a.rs", "file", json!({}), 1, 2))
            .unwrap();
        let graph_request = request(None, SemanticGraphGranularity::File, false, false);

        let first = storage.project_renderer_graph(&graph_request).unwrap();
        assert_eq!(db.graph.projection_cache_len(), 2);
        let cached = storage.project_renderer_graph(&graph_request).unwrap();
        assert_eq!(cached, first);
        assert_eq!(db.graph.projection_cache_len(), 2);

        storage
            .upsert_element(&element("b", "src/b.rs", "file", json!({}), 1, 2))
            .unwrap();
        assert_eq!(db.graph.projection_cache_len(), 0);
        let refreshed = storage.project_renderer_graph(&graph_request).unwrap();
        assert!(refreshed.commit_version > first.commit_version);
        assert_eq!(refreshed.nodes.len(), 2);
    }

    fn request(
        target_path: Option<&str>,
        granularity: SemanticGraphGranularity,
        include_external: bool,
        include_first_neighbors: bool,
    ) -> SemanticGraphProjectionRequest {
        SemanticGraphProjectionRequest {
            project_root: "/repo".into(),
            target_path: target_path.map(str::to_owned),
            granularity,
            recursive: true,
            include_external,
            include_first_neighbors,
        }
    }

    fn element(
        id: &str,
        path: &str,
        kind: &str,
        metadata: serde_json::Value,
        start_line: i64,
        end_line: i64,
    ) -> SemanticElement {
        SemanticElement {
            project_root: "/repo".into(),
            semantic_element_id: id.into(),
            semantic_source_id: "source".into(),
            path: path.into(),
            element_kind: kind.into(),
            name: id.into(),
            parent_element_id: None,
            content_fingerprint: None,
            start_line: Some(start_line),
            end_line: Some(end_line),
            lifecycle: "active".into(),
            match_evidence: None,
            metadata,
        }
    }

    fn relationship(
        source_element_id: &str,
        target_element_id: &str,
        relationship_kind: &str,
        label: &str,
    ) -> SemanticRelationship {
        SemanticRelationship {
            project_root: "/repo".into(),
            source_element_id: source_element_id.into(),
            target_element_id: target_element_id.into(),
            relationship_kind: relationship_kind.into(),
            label: label.into(),
            metadata: json!({}),
        }
    }
}
