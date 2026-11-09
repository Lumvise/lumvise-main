#![cfg(feature = "fastembed")]

use lumvise_neural_core::Text2VectorService;
use lumvise_neural_core::text2vector::FastEmbedText2VectorConfig;
use lumvise_neural_core::text2vector::FastEmbedText2VectorEngine;
use lumvise_neural_core::text2vector::Text2VectorRuntimeConfig;
use tempfile::TempDir;

#[test]
fn fastembed_text2vector_rejects_unknown_builtin_model_alias() {
    let error = match FastEmbedText2VectorEngine::new(FastEmbedText2VectorConfig::builtin(
        "fastembed",
        "not-a-real-embedding-model",
    )) {
        Ok(_) => panic!("expected unsupported model alias error"),
        Err(error) => error.to_string(),
    };

    assert!(error.contains("not-a-real-embedding-model"));
    assert!(error.contains("supported FastEmbed model"));
}

#[test]
fn fastembed_text2vector_reports_missing_local_model_files() {
    let temp = TempDir::new().unwrap();

    let error = match FastEmbedText2VectorEngine::new(FastEmbedText2VectorConfig::local_onnx(
        "fastembed-local",
        "bge-m3",
        temp.path().to_path_buf(),
    )) {
        Ok(_) => panic!("expected missing model file error"),
        Err(error) => error.to_string(),
    };

    assert!(error.contains("model.onnx"));
    assert!(error.contains("FastEmbed local ONNX model file"));
}

#[test]
fn text2vector_service_selects_fastembed_builtin_backend() {
    let error = match Text2VectorService::from_runtime_config(
        Text2VectorRuntimeConfig::fastembed_builtin("fastembed", "not-a-real-embedding-model"),
    ) {
        Ok(_) => panic!("expected unsupported model alias error"),
        Err(error) => error.to_string(),
    };

    assert!(error.contains("not-a-real-embedding-model"));
    assert!(error.contains("supported FastEmbed model"));
}

#[test]
fn text2vector_service_selects_fastembed_local_backend() {
    let temp = TempDir::new().unwrap();
    let config = Text2VectorRuntimeConfig::fastembed_local_onnx(
        "fastembed-local",
        "bge-m3",
        temp.path().to_path_buf(),
    );

    let error = match Text2VectorService::from_runtime_config(config) {
        Ok(_) => panic!("expected missing model file error"),
        Err(error) => error.to_string(),
    };

    assert!(error.contains("model.onnx"));
    assert!(error.contains("FastEmbed local ONNX model file"));
}

/// Real ONNX execution with deterministic token states [1, 0] and [0, 2].
/// Mean and CLS pooling yield different directions, independent of model weights.
struct PoolingOnnxFixture {
    assets: TempDir,
}

impl PoolingOnnxFixture {
    fn new() -> Self {
        use base64::Engine;
        let assets = TempDir::new().unwrap();
        // Constant graph, ONNX IR 8 / opset 13, checked with onnx.checker.
        let encoded = "CAg66gEKTRIRbGFzdF9oaWRkZW5fc3RhdGUiCENvbnN0YW50Ki4KBXZhbHVlKiIIAQgCCAIQASIQAACAPwAAAAAAAAAAAAAAQEIGdG9rZW5zoAEEEg9wb29saW5nX2ZpeHR1cmVaGwoJaW5wdXRfaWRzEg4KDAgHEggKAggBCgIIAlogCg5hdHRlbnRpb25fbWFzaxIOCgwIBxIICgIIAQoCCAJaIAoOdG9rZW5fdHlwZV9pZHMSDgoMCAcSCAoCCAEKAggCYicKEWxhc3RfaGlkZGVuX3N0YXRlEhIKEAgBEgwKAggBCgIIAgoCCAJCBAoAEA0=";
        let model = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        std::fs::write(assets.path().join("model.onnx"), model).unwrap();
        let fixture = Self { assets };
        fixture.write_tokenizer();
        fixture
    }

    fn write_tokenizer(&self) {
        let tokenizer = r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[PAD]":0,"[UNK]":1,"first":2,"second":3},"unk_token":"[UNK]"}}"#;
        for (name, content) in [
            ("tokenizer.json", tokenizer),
            ("config.json", r#"{"pad_token_id":0}"#),
            (
                "tokenizer_config.json",
                r#"{"model_max_length":512,"pad_token":"[PAD]"}"#,
            ),
            (
                "special_tokens_map.json",
                r#"{"pad_token":"[PAD]","unk_token":"[UNK]"}"#,
            ),
        ] {
            std::fs::write(self.assets.path().join(name), content).unwrap();
        }
    }

    fn embed(&self, model: &str) -> lumvise_neural_core::text2vector::Text2VectorResponse {
        let config = Text2VectorRuntimeConfig::fastembed_local_onnx(
            "fixture-engine",
            model,
            self.assets.path().into(),
        );
        Text2VectorService::from_runtime_config(config)
            .unwrap()
            .embed(&lumvise_neural_core::text2vector::Text2VectorRequest {
                text: "first second".into(),
                model: Some(model.into()),
            })
            .unwrap()
    }
}

#[test]
fn local_bge_models_use_cls_and_publish_a_new_vector_identity() {
    let fixture = PoolingOnnxFixture::new();
    for model in [
        "bge-small-en-v1.5",
        "bge-m3",
        "BAAI/bge-small-en-v1.5",
        "BAAI/bge-m3",
        "bgesmallenv15",
        "bge_small_en_v1.5",
    ] {
        let embedded = fixture.embed(model);
        assert_eq!(embedded.dimensions, 2);
        assert!(
            (embedded.vector[0] - 1.0).abs() < 1e-6,
            "{model}: {:?}",
            embedded.vector
        );
        assert!(
            embedded.vector[1].abs() < 1e-6,
            "{model}: {:?}",
            embedded.vector
        );
        assert!(
            embedded
                .metadata
                .engine_id
                .starts_with("fixture-engine::bge-cls-v1::profile-")
        );
        assert_eq!(embedded.metadata.model.as_deref(), Some(model));
    }
}

#[test]
fn local_minilm_and_custom_models_keep_mean_pooling_with_attested_identity() {
    let fixture = PoolingOnnxFixture::new();
    for model in [
        "all-minilm-l6-v2",
        "sentence-transformers/all-minilm-l6-v2",
        "custom-local-model",
    ] {
        let embedded = fixture.embed(model);
        let expected = [1.0 / 5.0_f32.sqrt(), 2.0 / 5.0_f32.sqrt()];
        assert_eq!(embedded.vector.len(), expected.len());
        for (actual, expected) in embedded.vector.iter().zip(expected) {
            assert!(
                (actual - expected).abs() < 1e-6,
                "{model}: {:?}",
                embedded.vector
            );
        }
        assert!(
            embedded
                .metadata
                .engine_id
                .starts_with("fixture-engine::profile-")
        );
    }
}

impl PoolingOnnxFixture {
    fn profile_engine(&self, max_length: usize) -> FastEmbedText2VectorEngine {
        let mut config = FastEmbedText2VectorConfig::local_onnx(
            "fixture-engine",
            "custom-local-model",
            self.assets.path().into(),
        );
        config.max_length = Some(max_length);
        FastEmbedText2VectorEngine::new(config).unwrap()
    }
}

fn vector_for(engine: &FastEmbedText2VectorEngine) -> lumvise_db_core::ArtifactTextVector {
    let response = engine
        .embed(&lumvise_neural_core::text2vector::Text2VectorRequest {
            text: "first second".into(),
            model: None,
        })
        .unwrap();
    lumvise_db_core::ArtifactTextVector {
        engine_id: response.metadata.engine_id,
        model: response.metadata.model,
        dimensions: response.dimensions,
        vector: response.vector,
        normalized: response.normalized,
        metadata: response.metadata.metadata,
    }
}

fn execute_vector_operation(
    db: &lumvise_db_core::LocalPersistence,
    operation: lumvise_db_core::SemanticOperation,
) -> lumvise_db_core::SemanticResult {
    use lumvise_db_core::SemanticPersistence;
    db.execute(
        operation,
        &lumvise_resource_routing::InvocationControl::sixty_seconds(),
    )
    .unwrap()
}

fn persist_profile_vector(
    db: &lumvise_db_core::LocalPersistence,
    vector: lumvise_db_core::ArtifactTextVector,
) {
    use lumvise_db_core::{SemanticElement, SemanticOperation, StoredSemanticElementTextVector};
    let element = SemanticElement {
        project_root: "/profile-canary".into(),
        semantic_element_id: "section".into(),
        semantic_source_id: "page".into(),
        path: "page.html".into(),
        element_kind: "html_section".into(),
        name: "Heading".into(),
        parent_element_id: None,
        content_fingerprint: Some("fp1:000000000000000a:000000000000000a".into()),
        start_line: Some(1),
        end_line: Some(1),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: serde_json::json!({}),
        revision_dependencies: vec![],
    };
    execute_vector_operation(
        db,
        SemanticOperation::SyncStructure {
            project_root: "/profile-canary".into(),
            elements: vec![element],
            relationships: vec![],
        },
    );
    execute_vector_operation(
        db,
        SemanticOperation::StoreElementTextVectors {
            project_root: "/profile-canary".into(),
            vectors: vec![StoredSemanticElementTextVector {
                semantic_element_id: "section".into(),
                project_root: "/profile-canary".into(),
                chunk: 0,
                content_fingerprint: "fp1:000000000000000a:000000000000000a".into(),
                vector,
            }],
        },
    );
}

fn profile_search_count(
    db: &lumvise_db_core::LocalPersistence,
    query: lumvise_db_core::ArtifactTextVector,
) -> usize {
    use lumvise_db_core::{SemanticOperation, SemanticResult};
    match execute_vector_operation(
        db,
        SemanticOperation::SearchElementTextVectors {
            project_root: "/profile-canary".into(),
            query: query.vector,
            k: 5,
            engine_id: query.engine_id,
            model: query.model,
        },
    ) {
        SemanticResult::ElementVectorSearch(results) => results.len(),
        other => panic!("expected element vector results, got {other:?}"),
    }
}

#[test]
fn changed_local_profile_excludes_previously_persisted_vectors() {
    let fixture = PoolingOnnxFixture::new();
    let old = vector_for(&fixture.profile_engine(512));
    let current = vector_for(&fixture.profile_engine(128));
    assert_ne!(old.engine_id, current.engine_id);
    let db = lumvise_db_core::LocalPersistence::in_memory().unwrap();
    persist_profile_vector(&db, old.clone());
    assert_eq!(profile_search_count(&db, old), 1);
    assert_eq!(profile_search_count(&db, current), 0);
}
