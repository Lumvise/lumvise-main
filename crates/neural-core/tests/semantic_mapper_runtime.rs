use lumvise_db_core::{
    LocalPersistence, SemanticArtifact, SemanticElement, SemanticOperation, SemanticPersistence,
    SemanticResult,
};
use lumvise_neural_core::semantic_mappers::{
    SemanticMapperMode, SemanticMapperService, SemanticMappingCandidate, SemanticMappingRequest,
    SemanticMappingResult, VectorEmbedder,
};
use lumvise_neural_core::{NeuralError, Result};
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use tempfile::TempDir;

struct NamedFakeVectorEmbedder {
    name: String,
    vectors: BTreeMap<String, Vec<f32>>,
}

impl NamedFakeVectorEmbedder {
    fn new(name: &str, vectors: &[(&str, Vec<f32>)]) -> Self {
        Self {
            name: name.to_string(),
            vectors: vectors
                .iter()
                .map(|(text, vector)| ((*text).to_string(), vector.clone()))
                .collect(),
        }
    }
}

impl VectorEmbedder for NamedFakeVectorEmbedder {
    fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        self.vectors
            .get(text)
            .cloned()
            .ok_or_else(|| NeuralError::InvalidValue {
                value: format!("{}:{text}", self.name),
                expected: "text registered in named fake vector embedder".to_string(),
            })
    }
}

struct PanicIfCalledVectorEmbedder;

impl VectorEmbedder for PanicIfCalledVectorEmbedder {
    fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        panic!("unexpected vector embedding call for {text}");
    }
}

#[test]
fn semantic_mapper_lexical_mode_ranks_by_token_overlap() {
    let mapper = SemanticMapperService::new(Arc::new(PanicIfCalledVectorEmbedder)).unwrap();
    let candidates = lexical_candidates();

    let results = mapper
        .map_candidates(
            SemanticMapperMode::Lexical,
            "semantic mapper runtime candidate content",
            &candidates,
        )
        .unwrap();

    assert_eq!(
        candidate_ids(&results),
        vec!["runtime-note", "semantic-note", "billing-note"]
    );
    assert!(results[0].score > results[1].score);
    assert!(results[0].explanation.contains("lexical token overlap"));
}

#[test]
fn semantic_mapper_lexical_mode_ignores_stop_words() {
    let mapper = SemanticMapperService::lexical();
    let candidates = vec![
        candidate("a-stopword", "the unrelated"),
        candidate("b-policy", "policy decision"),
    ];

    let results = mapper
        .map_candidates(SemanticMapperMode::Lexical, "the policy", &candidates)
        .unwrap();

    assert_eq!(candidate_ids(&results), vec!["b-policy", "a-stopword"]);
    assert!(results[0].score > results[1].score);
}

#[test]
fn semantic_mapper_request_can_select_configured_model() {
    let mapper = SemanticMapperService::lexical();
    let request = SemanticMappingRequest {
        mode: SemanticMapperMode::Lexical,
        source: "the policy".to_string(),
        candidates: vec![
            candidate("a-stopword", "the unrelated"),
            candidate("b-policy", "policy decision"),
        ],
        model: Some("lexical".to_string()),
    };

    let results = mapper.map_request(&request).unwrap();

    assert_eq!(candidate_ids(&results), vec!["b-policy", "a-stopword"]);
}

#[test]
fn semantic_mapper_request_rejects_unconfigured_model() {
    let mapper = SemanticMapperService::lexical();
    let request = SemanticMappingRequest {
        mode: SemanticMapperMode::Lexical,
        source: "policy".to_string(),
        candidates: vec![candidate("policy", "policy decision")],
        model: Some("arch-router".to_string()),
    };

    let error = mapper.map_request(&request).unwrap_err().to_string();

    assert!(error.contains("arch-router"));
    assert!(error.contains("configured semantic mapper model"));
}

#[test]
fn semantic_mapper_embedding_mode_ranks_by_cosine_similarity() {
    let mapper = SemanticMapperService::new(ranking_embedder("embedding-ranker")).unwrap();
    let candidates = vec![candidate("far-id", "far"), candidate("near-id", "near")];

    let results = mapper
        .map_candidates(SemanticMapperMode::BgeM3, "source", &candidates)
        .unwrap();

    assert_eq!(candidate_ids(&results), vec!["near-id", "far-id"]);
    assert!(results[0].score > results[1].score);
    assert!(results[0].explanation.contains("BgeM3 cosine similarity"));
}

#[test]
fn semantic_mapper_arch_router_model_spec_ranks_alias_matches() {
    let temp = TempDir::new().unwrap();
    write_router_spec(
        &temp,
        json!({
            "model_id": "arch-router-fixture",
            "aliases": { "policy": ["governance", "rule"] },
            "boosts": { "policy": 400 }
        }),
    );
    let mapper = SemanticMapperService::from_runtime_config(
        lumvise_neural_core::semantic_mappers::SemanticMapperRuntimeConfig::ArchRouter {
            model_dir: temp.path().to_path_buf(),
        },
    )
    .unwrap();
    let candidates = vec![
        candidate("plain", "cooldown timer only"),
        candidate("alias", "governance cooldown verification"),
    ];

    let results = mapper
        .map_candidates(
            SemanticMapperMode::ArchRouter,
            "policy cooldown",
            &candidates,
        )
        .unwrap();

    assert_eq!(candidate_ids(&results), vec!["alias", "plain"]);
    assert!(results[0].explanation.contains("arch-router-fixture"));
}

#[test]
fn semantic_mapper_arch_router_reports_missing_model_spec() {
    let temp = TempDir::new().unwrap();

    let error = match SemanticMapperService::from_runtime_config(
        lumvise_neural_core::semantic_mappers::SemanticMapperRuntimeConfig::ArchRouter {
            model_dir: temp.path().to_path_buf(),
        },
    ) {
        Ok(_) => panic!("expected missing model spec error"),
        Err(error) => error.to_string(),
    };

    assert!(error.contains("semantic-router.json or router.json"));
}

#[test]
fn semantic_mapper_rejects_unknown_mode() {
    let parse_error = SemanticMapperMode::parse("reranker-v0")
        .unwrap_err()
        .to_string();

    assert!(parse_error.contains("reranker-v0"));
    assert!(parse_error.contains("expected one of Lexical"));
}

#[test]
fn semantic_mapper_rejects_dimension_mismatch() {
    let mapper = SemanticMapperService::new(Arc::new(NamedFakeVectorEmbedder::new(
        "dimension-mismatch",
        &[("source", vec![1.0, 0.0]), ("bad", vec![1.0])],
    )))
    .unwrap();
    let mismatch = mapper
        .map_candidates(
            SemanticMapperMode::BgeM3,
            "source",
            &[candidate("bad-id", "bad")],
        )
        .unwrap_err()
        .to_string();

    assert!(mismatch.contains("2 vs 1"));
    assert!(mismatch.contains("expected vectors with equal dimensions"));
}

#[test]
fn semantic_mapper_returns_empty_for_empty_candidates() {
    let mapper = SemanticMapperService::new(Arc::new(PanicIfCalledVectorEmbedder)).unwrap();

    let results = mapper
        .map_candidates(SemanticMapperMode::ArchRouter, "source", &[])
        .unwrap();

    assert!(results.is_empty());
}

#[test]
fn semantic_mapper_db_core_candidates_read_artifacts_through_public_api() {
    let db = db_with_root_artifacts();
    let mapper = SemanticMapperService::new(ranking_embedder("db-artifact-ranker")).unwrap();

    let results = mapper
        .map_db_artifacts(SemanticMapperMode::BgeM3, "source", &db, "root")
        .unwrap();

    assert_eq!(candidate_ids(&results), vec!["artifact-a", "artifact-b"]);
}

#[test]
fn semantic_mapper_missing_db_core_element_returns_no_candidates() {
    let db = LocalPersistence::in_memory().unwrap();
    let mapper = SemanticMapperService::new(Arc::new(PanicIfCalledVectorEmbedder)).unwrap();

    let results = mapper
        .map_db_artifacts(SemanticMapperMode::Lexical, "source", &db, "missing")
        .unwrap();

    assert!(results.is_empty());
}

fn candidate(candidate_id: &str, content: &str) -> SemanticMappingCandidate {
    SemanticMappingCandidate {
        candidate_id: candidate_id.to_string(),
        content: content.to_string(),
        metadata: json!({}),
    }
}

fn lexical_candidates() -> Vec<SemanticMappingCandidate> {
    vec![
        candidate("billing-note", "invoice receipt payment"),
        candidate("runtime-note", "semantic mapper runtime candidate content"),
        candidate("semantic-note", "semantic mapper"),
    ]
}

fn ranking_embedder(name: &str) -> Arc<NamedFakeVectorEmbedder> {
    Arc::new(NamedFakeVectorEmbedder::new(name, &ranking_vectors()))
}

fn ranking_vectors() -> [(&'static str, Vec<f32>); 3] {
    [
        ("source", vec![1.0, 0.0]),
        ("near", vec![0.9, 0.1]),
        ("far", vec![0.0, 1.0]),
    ]
}

fn db_with_root_artifacts() -> LocalPersistence {
    let db = LocalPersistence::in_memory().unwrap();
    let control = InvocationControl::sixty_seconds();

    let result = db
        .execute(
            SemanticOperation::SyncStructure {
                project_root: "/repo".into(),
                elements: vec![element("root")],
                relationships: Vec::new(),
            },
            &control,
        )
        .expect("sync root semantic structure");
    assert!(matches!(result, SemanticResult::SyncStructure(_)));

    for (artifact_id, text) in [("artifact-a", "near"), ("artifact-b", "far")] {
        let result = db
            .execute(
                SemanticOperation::UpsertArtifact {
                    artifact: artifact(artifact_id, "root", text),
                    media_type: "text/plain".into(),
                },
                &control,
            )
            .unwrap_or_else(|error| panic!("upsert {artifact_id}: {error}"));
        assert!(
            matches!(result, SemanticResult::UpsertedArtifact { .. }),
            "upsert {artifact_id} returned unexpected result: {result:?}"
        );
    }

    db
}

fn artifact_metadata(text: &str) -> Value {
    json!({ "source_text": text })
}

fn candidate_ids(results: &[SemanticMappingResult]) -> Vec<&str> {
    results
        .iter()
        .map(|result| result.candidate_id.as_str())
        .collect()
}

fn element(id: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".to_string(),
        semantic_element_id: id.to_string(),
        semantic_source_id: "source".to_string(),
        path: id.to_string(),
        element_kind: "file".to_string(),
        name: id.to_string(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".to_string(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn artifact(id: &str, element_id: &str, text: &str) -> SemanticArtifact {
    SemanticArtifact {
        artifact_id: id.to_string(),
        semantic_element_id: element_id.to_string(),
        artifact_kind: "note".to_string(),
        title: id.to_string(),
        content_ref: None,
        content: Some(text.to_string()),
        searchable_text: Some(text.to_string()),
        content_size_bytes: Some(text.len()),
        dependencies: Vec::new(),
        metadata: artifact_metadata(text),
    }
}

fn write_router_spec(temp: &TempDir, value: serde_json::Value) {
    fs::write(temp.path().join("semantic-router.json"), value.to_string()).unwrap();
}
