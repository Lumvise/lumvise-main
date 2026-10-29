//! Engine-neutral exact ranking; adapters supply authorized candidates through the crate-root facade.
use crate::{ArtifactTextVector, DbError, Result, VectorSearchResult};
#[cfg(test)]
use std::cell::Cell;
use std::{collections::BinaryHeap, sync::Arc};
#[cfg(test)]
thread_local! { pub(crate) static SCORE_CALLS: Cell<usize> = const { Cell::new(0) }; }

/// Ranks authorized vectors with the same engine/model and cosine rules as local storage.
/// Example: `rank_vector_candidates(root, vectors, &[1.0, 0.0], 10, "engine", None)?`.
pub fn rank_vector_candidates<'a>(
    project_root: &str,
    candidates: impl IntoIterator<Item = (&'a str, &'a ArtifactTextVector)>,
    query: &[f32],
    k: usize,
    engine: &str,
    model: Option<&str>,
) -> Result<Vec<VectorSearchResult>> {
    validate_search_input(project_root, query, k)?;
    let candidates = candidates.into_iter().filter(|(_, vector)| {
        vector.engine_id == engine
            && vector.model.as_deref().unwrap_or("") == model.unwrap_or("")
            && vector.vector.iter().all(|value| value.is_finite())
    });
    Ok(top_k(
        candidates.map(|(id, vector)| (id.into(), Arc::from(vector.vector.as_slice()))),
        query,
        k,
    ))
}
pub(crate) fn validate_search_input(project_root: &str, query: &[f32], k: usize) -> Result<()> {
    if project_root.trim().is_empty() {
        return Err(DbError::invalid_value(
            project_root,
            "non-empty project root",
        ));
    }
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

pub(crate) fn top_k(
    candidates: impl IntoIterator<Item = (String, Arc<[f32]>)>,
    query: &[f32],
    k: usize,
) -> Vec<VectorSearchResult> {
    let mut heap = BinaryHeap::with_capacity(k);
    for (id, vector) in candidates {
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

pub(crate) fn cosine_score(query: &[f32], vector: &[f32]) -> f32 {
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

impl crate::SemanticArchive {
    /// Searches the archive's element-name vectors with local cosine ranking.
    /// Example: `archive.search_element_name_vectors(&query, 10, "engine", None)?`.
    pub fn search_element_name_vectors(
        &self,
        query: &[f32],
        k: usize,
        engine: &str,
        model: Option<&str>,
    ) -> Result<Vec<VectorSearchResult>> {
        rank_vector_candidates(
            &self.snapshot.project_root,
            self.element_name_vectors
                .iter()
                .map(|stored| (stored.semantic_element_id.as_str(), &stored.vector)),
            query,
            k,
            engine,
            model,
        )
    }
}
