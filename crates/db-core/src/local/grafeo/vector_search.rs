use crate::local::grafeo::semantic_storage::SemanticStorage;
use crate::local::sql::validation::require_non_empty;
use crate::{DbError, Result, VectorSearchResult};
use grafeo::{NodeId, Value as GrafeoValue};
use grafeo_core::graph::GraphStore as GrafeoGraphStore;
#[cfg(test)]
use std::cell::Cell;
use std::collections::{BinaryHeap, HashSet};
use std::sync::Arc;

#[cfg(test)]
thread_local! {
    static SCORE_CALLS: Cell<usize> = const { Cell::new(0) };
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
        Ok(self.graph.read(|graph| {
            let store = graph.graph_store();
            let project_nodes = graph
                .find_nodes_by_property("project_root", &GrafeoValue::from(project_root))
                .into_iter()
                .collect::<HashSet<_>>();
            let semantic_nodes = store
                .nodes_by_label("SemanticElement")
                .into_iter()
                .filter(|node_id| project_nodes.contains(node_id))
                .collect::<Vec<_>>();
            let owner_keys = vec!["semantic_element_id".into(), "lifecycle".into()];
            let owner_rows =
                store.get_nodes_properties_selective_batch(&semantic_nodes, &owner_keys);
            let owner_ids = owner_rows
                .into_iter()
                .filter_map(|properties| {
                    (batch_string(properties.get("lifecycle")).as_deref() != Some("inactive"))
                        .then(|| batch_string(properties.get("semantic_element_id")))
                        .flatten()
                })
                .collect::<HashSet<_>>();
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
                Some(&owner_ids),
            );
            top_k(candidates, query, k)
        }))
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
        Ok(self.graph.read(|graph| {
            let project_nodes = graph
                .find_nodes_by_property("project_root", &GrafeoValue::from(project_root))
                .into_iter()
                .collect::<HashSet<_>>();
            let store = graph.graph_store();
            let node_ids = store
                .nodes_by_label(ELEMENT_NAME_VECTOR_LABEL)
                .into_iter()
                .filter(|node_id| project_nodes.contains(node_id))
                .collect::<Vec<_>>();
            let candidates = vector_candidates_from_properties(
                &*store,
                &node_ids,
                "semantic_element_id",
                engine_id,
                model,
                None,
            );
            top_k(candidates, query, k)
        }))
    }
}

fn validate_search_input(project_root: &str, query: &[f32], k: usize) -> Result<()> {
    require_non_empty(project_root, "non-empty project root")?;
    if query.is_empty() {
        return Err(DbError::invalid_value("[]", "non-empty vector query"));
    }
    if !query.iter().all(|value| value.is_finite()) {
        return Err(DbError::invalid_value(
            "query",
            "finite vector query values",
        ));
    }
    if k == 0 {
        return Err(DbError::invalid_value("0", "top-k greater than zero"));
    }
    Ok(())
}

fn vector_candidates_from_properties(
    store: &dyn GrafeoGraphStore,
    node_ids: &[NodeId],
    id_property: &str,
    engine_id: &str,
    model: Option<&str>,
    owner_ids: Option<&HashSet<String>>,
) -> Vec<(NodeId, String, Arc<[f32]>)> {
    let mut keys = vec![
        id_property.into(),
        "engine_id".into(),
        "model".into(),
        VECTOR_PROPERTY.into(),
    ];
    if owner_ids.is_some() {
        keys.push("semantic_element_id".into());
    }
    let rows = store.get_nodes_properties_selective_batch(node_ids, &keys);
    node_ids
        .iter()
        .enumerate()
        .filter_map(|(row, node_id)| {
            let properties = rows.get(row)?;
            let id = batch_string(properties.get(id_property))?;
            if batch_string(properties.get("engine_id")).as_deref() != Some(engine_id)
                || batch_string(properties.get("model")).as_deref() != model.or(Some(""))
            {
                return None;
            }
            if let Some(allowed) = owner_ids {
                let owner = batch_string(properties.get("semantic_element_id"));
                if !owner.is_some_and(|owner| allowed.contains(&owner)) {
                    return None;
                }
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

#[derive(Debug)]
struct RankedCandidate {
    id: String,
    score: f32,
}

impl PartialEq for RankedCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.score.to_bits() == other.score.to_bits() && self.id == other.id
    }
}

impl Eq for RankedCandidate {}

impl PartialOrd for RankedCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedCandidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then_with(|| self.id.cmp(&other.id))
    }
}

fn top_k(
    candidates: impl IntoIterator<Item = (NodeId, String, Arc<[f32]>)>,
    query: &[f32],
    k: usize,
) -> Vec<VectorSearchResult> {
    let mut heap = BinaryHeap::with_capacity(k);
    for (_, id, vector) in candidates {
        if vector.len() != query.len() {
            continue;
        }
        let score = cosine_score(query, &vector);
        heap.push(RankedCandidate { id, score });
        if heap.len() > k {
            heap.pop();
        }
    }
    let mut ranked = heap.into_vec();
    ranked.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then(left.id.cmp(&right.id))
    });
    ranked
        .into_iter()
        .map(|candidate| VectorSearchResult {
            id: candidate.id,
            score: candidate.score,
        })
        .collect()
}

fn cosine_score(query: &[f32], vector: &[f32]) -> f32 {
    #[cfg(test)]
    SCORE_CALLS.with(|calls| calls.set(calls.get() + 1));
    let (dot, query_norm, vector_norm) = query.iter().zip(vector).fold(
        (0.0_f32, 0.0_f32, 0.0_f32),
        |(dot, query_norm, vector_norm), (query, vector)| {
            (
                dot + query * vector,
                query_norm + query * query,
                vector_norm + vector * vector,
            )
        },
    );
    let denominator = query_norm.sqrt() * vector_norm.sqrt();
    if denominator == 0.0 {
        0.0
    } else {
        dot / denominator
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
        storage
            .commit_graph_write(|graph, _, _| {
                crate::local::grafeo::graph_rows::upsert_artifact_vector_node(
                    graph,
                    &project_artifact,
                    "project",
                    &vector_record(vec![1.0, 0.0]),
                )?;
                crate::local::grafeo::graph_rows::upsert_artifact_vector_node(
                    graph,
                    &other_artifact,
                    "other",
                    &vector_record(vec![1.0, 0.0]),
                )
            })
            .unwrap();

        let results = storage
            .search_artifact_text_vectors("/project", &[1.0, 0.0], 1, "test", None)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "project-artifact");
    }

    fn element(id: &str, name: &str) -> SemanticElement {
        SemanticElement {
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
