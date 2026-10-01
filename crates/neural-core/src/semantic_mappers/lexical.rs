use crate::semantic_mappers::text_profile::{TextProfile, lexical_score};
use crate::semantic_mappers::{SemanticMappingCandidate, SemanticMappingResult};
use serde_json::json;

pub fn rank(source: &str, candidates: &[SemanticMappingCandidate]) -> Vec<SemanticMappingResult> {
    let query = TextProfile::new(source);
    let mut results: Vec<SemanticMappingResult> = candidates
        .iter()
        .map(|candidate| lexical_result(candidate, &query))
        .collect();
    results.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.candidate_id.cmp(&right.candidate_id))
    });
    results
}

fn lexical_result(
    candidate: &SemanticMappingCandidate,
    query: &TextProfile,
) -> SemanticMappingResult {
    let score = lexical_score(query, &candidate.content);
    SemanticMappingResult {
        candidate_id: candidate.candidate_id.clone(),
        score: score as f32,
        explanation: format!("lexical token overlap and semantic score {score}"),
        metadata: json!({ "mode": "Lexical" }),
    }
}
