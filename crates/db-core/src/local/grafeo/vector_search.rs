#[cfg(test)]
use crate::domain::vector_search::{SCORE_CALLS, cosine_score};
use crate::domain::vector_search::{top_k as rank_vectors, validate_search_input};
use crate::local::grafeo::committed_columns::CommittedPropertyRows;
use crate::local::grafeo::graph_rows::{
    artifact_is_current_in_project, semantic_element_by_id_selective,
};
use crate::local::grafeo::semantic_storage::SemanticStorage;
use crate::{Result, VectorSearchResult};
use grafeo::{NodeId, Value as GrafeoValue};
use grafeo_core::graph::GraphStore as GrafeoGraphStore;
#[cfg(test)]
use std::cell::Cell;
use std::sync::Arc;

fn top_k(
    candidates: impl IntoIterator<Item = (NodeId, String, Arc<[f32]>)>,
    query: &[f32],
    k: usize,
) -> Vec<VectorSearchResult> {
    rank_vectors(
        candidates.into_iter().map(|(_, id, vector)| (id, vector)),
        query,
        k,
    )
}

const ARTIFACT_VECTOR_LABEL: &str = "SemanticArtifactVector";
const ELEMENT_NAME_VECTOR_LABEL: &str = "SemanticElementNameVector";
const VECTOR_PROPERTY: &str = "vector";

impl<'db> SemanticStorage<'db> {
    /// Returns exact cosine top-k artifact text vectors in one project.
    ///
    /// Candidates are scored once and retained in a bounded heap, so ranking
    /// memory is proportional to `k` rather than the number of stored vectors.
    pub fn search_artifact_text_vectors(
        &self,
        project_root: &str,
        query: &[f32],
        k: usize,
        engine_id: &str,
        model: Option<&str>,
    ) -> Result<Vec<VectorSearchResult>> {
        validate_search_input(project_root, query, k)?;
        self.graph.stable_read(|graph| {
            let store = graph.graph_store();
            let node_ids = store
                .nodes_by_label(ARTIFACT_VECTOR_LABEL)
                .into_iter()
                .collect::<Vec<_>>();
            let candidates = vector_candidates_from_properties(
                &*store,
                &node_ids,
                "artifact_id",
                engine_id,
                model,
                |id| artifact_is_current_in_project(graph, project_root, id),
            );
            Ok(top_k(candidates, query, k))
        })
    }

    /// Returns the exact cosine top-k semantic element-name vectors in one project.
    pub fn search_element_name_vectors(
        &self,
        project_root: &str,
        query: &[f32],
        k: usize,
        engine_id: &str,
        model: Option<&str>,
    ) -> Result<Vec<VectorSearchResult>> {
        validate_search_input(project_root, query, k)?;
        self.graph.stable_read(|graph| {
            let store = graph.graph_store();
            let node_ids = store
                .nodes_by_label(ELEMENT_NAME_VECTOR_LABEL)
                .into_iter()
                .collect::<Vec<_>>();
            let candidates = vector_candidates_from_properties(
                &*store,
                &node_ids,
                "semantic_element_id",
                engine_id,
                model,
                |id| {
                    semantic_element_by_id_selective(graph, id)
                        .is_some_and(|element| element.project_root == project_root)
                },
            );
            Ok(top_k(candidates, query, k))
        })
    }
}

fn vector_candidates_from_properties(
    store: &dyn GrafeoGraphStore,
    node_ids: &[NodeId],
    id_property: &str,
    engine_id: &str,
    model: Option<&str>,
    include_record: impl Fn(&str) -> bool,
) -> Vec<(NodeId, String, Arc<[f32]>)> {
    let keys = vec![
        id_property.into(),
        "engine_id".into(),
        "model".into(),
        VECTOR_PROPERTY.into(),
    ];
    let rows = store
        .get_nodes_properties_selective_batch(node_ids, &keys)
        .committed_nodes(store, node_ids);
    node_ids
        .iter()
        .enumerate()
        .filter_map(|(row, node_id)| {
            let properties = rows.get(row)?;
            let id = batch_string(properties.get(id_property))?;
            if batch_string(properties.get("engine_id")).as_deref() != Some(engine_id)
                || batch_string(properties.get("model")).as_deref() != model.or(Some(""))
                || !include_record(&id)
            {
                return None;
            }
            let GrafeoValue::Vector(vector) = properties.get(VECTOR_PROPERTY)? else {
                return None;
            };
            vector
                .iter()
                .all(|value| value.is_finite())
                .then(|| (*node_id, id, Arc::clone(vector)))
        })
        .collect()
}

fn batch_string(value: Option<&GrafeoValue>) -> Option<String> {
    match value? {
        GrafeoValue::String(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::grafeo::graph_rows::string_property;
    use crate::{ArtifactTextVector, SemanticArtifact, SemanticElement};
    use serde_json::json;

    #[test]
    fn native_vector_round_trip_and_deterministic_element_top_k() {
        let db = crate::local::runtime::DbCore::in_memory().unwrap();
        let storage = db.storage_manager().semantic_storage();
        for (id, name, vector) in [
            ("a", "Alpha", vec![1.0, 0.0]),
            ("b", "Beta", vec![0.9, 0.1]),
            ("c", "Gamma", vec![0.0, 1.0]),
        ] {
            let element = element(id, name);
            storage.upsert_element(&element).unwrap();
            storage
                .commit_graph_write(|graph, _, _| {
                    crate::local::grafeo::graph_rows::upsert_element_name_vector_node(
                        graph,
                        &element,
                        name,
                        &vector_record(vector),
                    )
                })
                .unwrap();
        }
        storage
            .commit_graph_write(|graph, _, _| {
                for _ in 0..2_048 {
                    graph.create_node_with_props(
                        &["ProjectNoise"],
                        vec![("project_root", GrafeoValue::from("/project"))],
                    )?;
                }
                for index in 0..2_048 {
                    let mut unrelated = element(&format!("other-{index}"), "Other");
                    unrelated.project_root = "/unrelated".into();
                    let props =
                        crate::local::grafeo::graph_row_projection::element_name_vector_props(
                            &unrelated,
                            "other",
                            &vector_record(vec![1.0, 0.0]),
                        )?;
                    graph.create_node_with_props(&[ELEMENT_NAME_VECTOR_LABEL], props)?;
                }
                Ok(())
            })
            .unwrap();

        let stored = storage.element_name_vector("a").unwrap().unwrap();
        assert_eq!(stored.vector.vector, vec![1.0, 0.0]);
        SCORE_CALLS.with(|calls| calls.set(0));
        let results = storage
            .search_element_name_vectors("/project", &[1.0, 0.0], 2, "test", None)
            .unwrap();
        assert_eq!(
            results
                .iter()
                .map(|result| result.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(SCORE_CALLS.with(Cell::get), 3);
        assert!((results[0].score - 1.0).abs() < 1e-5);
        assert!((results[1].score - 0.993_883_7).abs() < 1e-5);
        db.graph.read(|graph| {
            let node = graph
                .iter_nodes()
                .find(|node| node.has_label(ELEMENT_NAME_VECTOR_LABEL) && string_property(node, "semantic_element_id").as_deref() == Some("a"))
                .unwrap();
            assert!(matches!(node.get_property(VECTOR_PROPERTY), Some(GrafeoValue::Vector(vector)) if vector.as_ref() == [1.0, 0.0]));
        });
    }
    #[test]
    fn exact_top_k_scores_each_eligible_candidate_once_and_is_order_stable() {
        let mut candidates = (0..2_000)
            .map(|index| {
                (
                    NodeId::new(index as u64),
                    format!("id-{index:04}"),
                    Arc::<[f32]>::from([1.0, (index % 17) as f32 / 100.0]),
                )
            })
            .collect::<Vec<_>>();
        candidates.push((
            NodeId::new(9_999),
            "wrong-dimension".into(),
            Arc::<[f32]>::from([1.0]),
        ));
        SCORE_CALLS.with(|calls| calls.set(0));
        let first = top_k(candidates.clone(), &[1.0, 0.0], 7);
        candidates.reverse();
        let second = top_k(candidates, &[1.0, 0.0], 7);
        assert_eq!(SCORE_CALLS.with(Cell::get), 4_000);
        assert_eq!(first, second);
        assert_eq!(
            first
                .iter()
                .map(|result| result.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "id-0000", "id-0017", "id-0034", "id-0051", "id-0068", "id-0085", "id-0102"
            ]
        );
    }

    #[test]
    fn cosine_zero_vectors_are_finite_zero_score_and_ties_sort_by_id() {
        assert_eq!(cosine_score(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
        let mut candidates = vec![
            (NodeId::new(1), "z".into(), Arc::<[f32]>::from([1.0, 0.0])),
            (NodeId::new(2), "a".into(), Arc::<[f32]>::from([1.0, 0.0])),
        ];
        candidates.reverse();
        let results = top_k(candidates, &[1.0, 0.0], 2);
        assert_eq!(results[0].id, "a");
        assert_eq!(results[1].id, "z");
        assert_eq!(results[0].score, results[1].score);
    }

    #[test]
    fn artifact_top_k_is_scoped_to_its_project() {
        let db = crate::local::runtime::DbCore::in_memory().unwrap();
        let storage = db.storage_manager().semantic_storage();
        let project_element = element("project-element", "Project");
        let other_element = SemanticElement {
            revision_dependencies: Vec::new(),
            project_root: "/other".into(),
            semantic_element_id: "other-element".into(),
            semantic_source_id: "other-element".into(),
            path: "other.rs".into(),
            element_kind: "function".into(),
            name: "Other".into(),
            parent_element_id: None,
            content_fingerprint: None,
            start_line: None,
            end_line: None,
            lifecycle: "active".into(),
            match_evidence: None,
            metadata: json!({}),
        };
        storage.upsert_element(&project_element).unwrap();
        storage.upsert_element(&other_element).unwrap();
        let project_artifact = artifact("project-artifact", "project-element");
        let other_artifact = artifact("other-artifact", "other-element");
        for (root, artifact) in [("/project", project_artifact), ("/other", other_artifact)] {
            storage.upsert_artifact(&artifact).unwrap();
            storage
                .store_artifact_text_vectors(
                    root,
                    &[crate::StoredArtifactTextVector {
                        artifact_id: artifact.artifact_id.clone(),
                        semantic_element_id: artifact.semantic_element_id.clone(),
                        source_text: artifact.searchable_text.unwrap(),
                        vector: vector_record(vec![1.0, 0.0]),
                    }],
                )
                .unwrap();
        }
        storage
            .commit_graph_write(|graph, _, _| {
                crate::local::grafeo::graph_rows::upsert_artifact_vector_node(
                    graph,
                    &artifact("orphan", "project-element"),
                    "orphan",
                    &vector_record(vec![1.0, 0.0]),
                )
            })
            .unwrap();

        let results = storage
            .search_artifact_text_vectors("/project", &[1.0, 0.0], 10, "test", None)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "project-artifact");
    }

    fn element(id: &str, name: &str) -> SemanticElement {
        SemanticElement {
            revision_dependencies: Vec::new(),
            project_root: "/project".into(),
            semantic_element_id: id.into(),
            semantic_source_id: id.into(),
            path: format!("{id}.rs"),
            element_kind: "function".into(),
            name: name.into(),
            parent_element_id: None,
            content_fingerprint: None,
            start_line: None,
            end_line: None,
            lifecycle: "active".into(),
            match_evidence: None,
            metadata: json!({}),
        }
    }

    fn artifact(id: &str, semantic_element_id: &str) -> SemanticArtifact {
        SemanticArtifact {
            artifact_id: id.into(),
            semantic_element_id: semantic_element_id.into(),
            artifact_kind: "note".into(),
            title: id.into(),
            content_ref: None,
            content: None,
            searchable_text: Some(id.into()),
            content_size_bytes: None,
            dependencies: vec![],
            metadata: json!({}),
        }
    }

    fn vector_record(vector: Vec<f32>) -> ArtifactTextVector {
        ArtifactTextVector {
            engine_id: "test".into(),
            model: None,
            dimensions: vector.len(),
            vector,
            normalized: false,
            metadata: json!({}),
        }
    }
}
