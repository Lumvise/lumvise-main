//! Explicit offline canaries using an installed local ONNX/tokenizer, never downloads.
use super::*;

#[test]
#[ignore = "requires explicit LUMVISE_TEST_NOMIC_MODEL_DIR; no downloads"]
fn installed_nomic_batch_port_timing_and_scalar_agreement() {
    use lumvise_db_core::ArtifactTextVectorizer;
    let directory = std::env::var_os("LUMVISE_TEST_NOMIC_MODEL_DIR")
        .expect("expected installed Nomic directory");
    let service = crate::Text2VectorService::from_runtime_config(
        crate::Text2VectorRuntimeConfig::FastEmbedLocalOnnx {
            engine_id: "nomic-timing-canary".into(),
            model_id: "nomic-embed-text-v1.5".into(),
            model_dir: directory.into(),
        },
    )
    .unwrap();
    service
        .vectorize_artifact_text("warm up", TextEmbeddingTask::Document)
        .unwrap();
    compare_batch_workload(
        &service,
        "short_names",
        &(0..32)
            .map(|n| format!("lookup semantic element {n}"))
            .collect::<Vec<_>>(),
    );
    let mut mixed = vec!["small heading".to_owned(); 7];
    mixed.push("This document describes source indexing and embedding vector storage. ".repeat(80));
    compare_batch_workload(&service, "mixed_lengths", &mixed);
}

fn compare_batch_workload(service: &crate::Text2VectorService, name: &str, texts: &[String]) {
    use lumvise_db_core::ArtifactTextVectorizer;
    let input = texts.iter().map(String::as_str).collect::<Vec<_>>();
    let start = std::time::Instant::now();
    let scalar = input
        .iter()
        .map(|text| {
            service
                .vectorize_artifact_text(text, TextEmbeddingTask::Document)
                .unwrap()
        })
        .collect::<Vec<_>>();
    let scalar_ms = start.elapsed().as_millis();
    let start = std::time::Instant::now();
    let batch = service
        .vectorize_artifact_texts(&input, TextEmbeddingTask::Document)
        .unwrap();
    let batch_ms = start.elapsed().as_millis();
    assert_eq!(scalar.len(), batch.len());
    let similarities = scalar
        .iter()
        .zip(&batch)
        .map(|(left, right)| cosine(&left.vector, &right.vector))
        .collect::<Vec<_>>();
    println!(
        "{}",
        serde_json::json!({"workload":name,"items":input.len(),"scalar_ms":scalar_ms,"batch_ms":batch_ms,"concurrent_host_load":true,"cosines":similarities})
    );
    for (index, similarity) in similarities.iter().enumerate() {
        assert!(
            *similarity > 0.99,
            "{name} item {index} cosine {similarity}; expected >0.99 scalar/batch agreement"
        );
    }
}

#[test]
#[ignore = "requires explicit LUMVISE_TEST_VECTOR_MODEL_DIR; no downloads"]
fn installed_batch_port_keeps_order_dimensions_and_profile() {
    let engine = installed_engine(false);
    let texts = [
        "fresh bread from the oven",
        "quantum entanglement experiments",
        "birds fly south in winter",
    ]
    .into_iter()
    .cycle()
    .take(19)
    .collect::<Vec<_>>();
    let batch = engine
        .embed_batch(&texts, TextEmbeddingTask::Document)
        .unwrap();
    assert_eq!(batch.len(), 19);
    for (index, text) in texts.iter().enumerate() {
        let single = engine
            .embed(&Text2VectorRequest {
                text: (*text).into(),
                model: None,
            })
            .unwrap();
        assert_eq!(batch[index].metadata.engine_id, single.metadata.engine_id);
        assert_eq!(batch[index].metadata.model, single.metadata.model);
        assert_eq!(batch[index].dimensions, single.dimensions);
        assert!(
            cosine(&batch[index].vector, &single.vector) > 0.99,
            "batch item {index} lost its source association"
        );
    }
    assert!(
        engine
            .embed_batch(&[], TextEmbeddingTask::Document)
            .unwrap()
            .is_empty()
    );
    assert!(cosine(&batch[0].vector, &batch[1].vector) < 0.95);
}

fn cosine(left: &[f32], right: &[f32]) -> f32 {
    let dot = left.iter().zip(right).map(|(a, b)| a * b).sum::<f32>();
    let norm = |v: &[f32]| v.iter().map(|a| a * a).sum::<f32>().sqrt();
    dot / (norm(left) * norm(right))
}

#[test]
#[ignore = "requires explicit LUMVISE_TEST_NOMIC_MODEL_DIR; no downloads"]
fn installed_nomic_batch_port_keeps_512_dimensions_profile_task_and_source_order() {
    use lumvise_db_core::ArtifactTextVectorizer;
    let directory = std::env::var_os("LUMVISE_TEST_NOMIC_MODEL_DIR")
        .expect("expected installed Nomic directory");
    let service = crate::Text2VectorService::from_runtime_config(
        crate::Text2VectorRuntimeConfig::FastEmbedLocalOnnx {
            engine_id: "nomic-batch-canary".into(),
            model_id: "nomic-embed-text-v1.5".into(),
            model_dir: directory.into(),
        },
    )
    .unwrap();
    let texts = [
        "fresh bread from the oven",
        "quantum entanglement experiments",
        "birds fly south in winter",
    ];
    for task in [TextEmbeddingTask::Document, TextEmbeddingTask::Query] {
        let batch = service.vectorize_artifact_texts(&texts, task).unwrap();
        assert_eq!(batch.len(), 3);
        for (index, text) in texts.iter().enumerate() {
            let scalar = service.vectorize_artifact_text(text, task).unwrap();
            assert_eq!(batch[index].dimensions, 512);
            assert_eq!(batch[index].vector.len(), 512);
            assert_eq!(batch[index].engine_id, scalar.engine_id);
            assert!(batch[index].engine_id.contains("::profile-"));
            assert!(cosine(&batch[index].vector, &scalar.vector) > 0.99);
        }
    }
}

fn installed_engine(nomic_prefix: bool) -> FastEmbedText2VectorEngine {
    let directory = std::env::var_os("LUMVISE_TEST_VECTOR_MODEL_DIR")
        .expect("set LUMVISE_TEST_VECTOR_MODEL_DIR to an installed local MiniLM directory");
    // The installed tokenizer exercises Nomic prefixes; this is not Nomic inference proof.
    let alias = if nomic_prefix {
        "nomic-embed-text-v1.5"
    } else {
        "all-minilm-l6-v2"
    };
    FastEmbedText2VectorEngine::new(FastEmbedText2VectorConfig::local_onnx(
        "offline-canary",
        alias,
        directory.into(),
    ))
    .unwrap()
}

#[test]
#[ignore = "requires explicit LUMVISE_TEST_VECTOR_MODEL_DIR; no downloads"]
fn installed_tokenizer_covers_utf8_tail_with_prefix_in_budget() {
    let engine = installed_engine(true);
    let source = format!(
        "  {} terminal-tail-fact\n",
        "聲學 Ωmega uncharacteristically; ".repeat(300)
    );
    let request = TextChunkPlanRequest {
        text: source.clone(),
        task: TextEmbeddingTask::Document,
        max_tokens: 32,
    };
    let plan = engine.plan_text_chunks(&request).unwrap();
    assert!(plan.ranges.len() > 2);
    let mut cursor = 0;
    for range in &plan.ranges {
        assert_eq!(range.start_byte, cursor);
        assert!(range.end_byte > cursor && source.is_char_boundary(range.end_byte));
        let prepared = format!(
            "search_document: {}",
            &source[range.start_byte..range.end_byte]
        );
        let mut tokenizer = engine.model.lock().unwrap().tokenizer.clone();
        tokenizer.with_truncation(None).unwrap();
        tokenizer.with_padding(None);
        let actual = tokenizer.encode(prepared.as_str(), true).unwrap().len();
        assert_eq!(range.token_count, actual);
        assert!(actual <= 32);
        cursor = range.end_byte;
    }
    assert_eq!(cursor, source.len());
    let last = plan.ranges.last().unwrap();
    assert!(source[last.start_byte..].contains("terminal-tail-fact"));
}

#[test]
#[ignore = "requires explicit LUMVISE_TEST_VECTOR_MODEL_DIR; no downloads"]
fn planning_preserves_inference_configuration_and_embedding() {
    let engine = installed_engine(false);
    let request = Text2VectorRequest {
        text: "sound travels through water".into(),
        model: None,
    };
    let before = engine.embed(&request).unwrap().vector;
    let tokenizer_before = engine
        .model
        .lock()
        .unwrap()
        .tokenizer
        .to_string(false)
        .unwrap();
    let plan = engine
        .plan_text_chunks(&TextChunkPlanRequest {
            text: "water ".repeat(1200),
            task: TextEmbeddingTask::Document,
            max_tokens: 32,
        })
        .unwrap();
    assert!(plan.ranges.len() > 1);
    assert_eq!(
        tokenizer_before,
        engine
            .model
            .lock()
            .unwrap()
            .tokenizer
            .to_string(false)
            .unwrap()
    );
    assert_eq!(before, engine.embed(&request).unwrap().vector);
}

#[test]
#[ignore = "requires explicit LUMVISE_TEST_VECTOR_MODEL_DIR; no downloads"]
fn installed_tokenizer_handles_empty_dense_long_word_and_invalid_budget() {
    let engine = installed_engine(true);
    for text in ["", " \n\t"] {
        let result = engine
            .plan_text_chunks(&TextChunkPlanRequest {
                text: text.into(),
                task: TextEmbeddingTask::Query,
                max_tokens: 32,
            })
            .unwrap();
        assert!(result.ranges.is_empty());
    }
    let text = format!("{}{}tail", "ß".repeat(500), "!?".repeat(900));
    let plan = engine
        .plan_text_chunks(&TextChunkPlanRequest {
            text: text.clone(),
            task: TextEmbeddingTask::Query,
            max_tokens: 24,
        })
        .unwrap();
    let rebuilt = plan
        .ranges
        .iter()
        .map(|range| &text[range.start_byte..range.end_byte])
        .collect::<String>();
    assert_eq!(rebuilt, text);
    assert!(plan.ranges.iter().all(|range| range.token_count <= 24));
    let error = engine
        .plan_text_chunks(&TextChunkPlanRequest {
            text: "private-body".into(),
            task: TextEmbeddingTask::Document,
            max_tokens: 0,
        })
        .unwrap_err();
    assert!(!error.to_string().contains("private-body"));
}

#[test]
#[ignore = "requires explicit LUMVISE_TEST_VECTOR_MODEL_DIR; no downloads"]
fn installed_profile_changes_with_context_and_prefix_recipe() {
    let plain = installed_engine(false);
    let second_plain = installed_engine(false);
    assert_eq!(
        plain.embedding_profile_fingerprint().unwrap(),
        second_plain.embedding_profile_fingerprint().unwrap()
    );
    let nomic = installed_engine(true);
    assert_ne!(
        plain.embedding_profile_fingerprint().unwrap(),
        nomic.embedding_profile_fingerprint().unwrap()
    );
    let directory = std::env::var_os("LUMVISE_TEST_VECTOR_MODEL_DIR").unwrap();
    let mut config = FastEmbedText2VectorConfig::local_onnx(
        "offline-canary",
        "all-minilm-l6-v2",
        directory.into(),
    );
    config.max_length = Some(128);
    let shorter = FastEmbedText2VectorEngine::new(config).unwrap();
    assert_ne!(
        plain.embedding_profile_fingerprint().unwrap(),
        shorter.embedding_profile_fingerprint().unwrap()
    );
}
