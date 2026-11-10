use super::*;
use lumvise_db_core::ArtifactTextVector;
use std::sync::Mutex;

struct NamedBatchVectorizer {
    calls: Arc<Mutex<Vec<TextEmbeddingTask>>>,
    malformed: bool,
}
impl ArtifactTextVectorizer for NamedBatchVectorizer {
    fn vectorize_artifact_text(
        &self,
        text: &str,
        _: TextEmbeddingTask,
    ) -> lumvise_db_core::Result<ArtifactTextVector> {
        assert_eq!(text, "lumvise-vector-engine-identity-probe");
        Ok(batch_vector("first"))
    }
    fn vectorize_artifact_texts(
        &self,
        texts: &[&str],
        task: TextEmbeddingTask,
    ) -> lumvise_db_core::Result<Vec<ArtifactTextVector>> {
        self.calls.lock().unwrap().push(task);
        if self.malformed {
            return Ok(Vec::new());
        }
        Ok(texts.iter().map(|text| batch_vector(text)).collect())
    }
}

fn batch_vector(text: &str) -> ArtifactTextVector {
    ArtifactTextVector {
        engine_id: "batch".into(),
        model: None,
        dimensions: 2,
        vector: if text == "first" {
            vec![1.0, 0.0]
        } else {
            vec![0.0, 1.0]
        },
        normalized: false,
        metadata: json!({}),
    }
}

#[test]
fn neural_embed_uses_one_ordered_batch_and_preserves_query_task() {
    let app = app_with_default_broker();
    let calls = Arc::new(Mutex::new(Vec::new()));
    app.plugin_endpoints()
        .set_plugin_vectorizer(Box::new(NamedBatchVectorizer {
            calls: calls.clone(),
            malformed: false,
        }))
        .unwrap();
    let broker = app.default_plugin_host_capability_broker().unwrap();
    let value = broker
        .invoke(request(
            "plugin.semantic",
            NEURAL_EMBED_CAPABILITY,
            json!({"texts":["second","first"],"task":"query"}),
        ))
        .unwrap();
    assert_eq!(value["vectors"], json!([[0.0, 1.0], [1.0, 0.0]]));
    assert_eq!(*calls.lock().unwrap(), vec![TextEmbeddingTask::Query]);
}

#[test]
fn neural_embed_rejects_a_short_batch_before_returning_vectors() {
    let app = app_with_default_broker();
    app.plugin_endpoints()
        .set_plugin_vectorizer(Box::new(NamedBatchVectorizer {
            calls: Arc::default(),
            malformed: true,
        }))
        .unwrap();
    let broker = app.default_plugin_host_capability_broker().unwrap();
    let failure = broker
        .invoke(request(
            "plugin.semantic",
            NEURAL_EMBED_CAPABILITY,
            json!({"texts":["secret-first","secret-second"]}),
        ))
        .unwrap_err();
    assert!(failure.to_string().contains("expected 2"));
    assert!(!failure.to_string().contains("secret-first"));
}
