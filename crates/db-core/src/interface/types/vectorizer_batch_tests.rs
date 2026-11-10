use crate::{ArtifactTextVector, ArtifactTextVectorizer, DbError, TextEmbeddingTask};
use std::sync::Mutex;

#[derive(Default)]
struct NamedScalarVectorizer {
    calls: Mutex<Vec<(String, TextEmbeddingTask)>>,
}
impl ArtifactTextVectorizer for NamedScalarVectorizer {
    fn vectorize_artifact_text(
        &self,
        text: &str,
        task: TextEmbeddingTask,
    ) -> crate::Result<ArtifactTextVector> {
        self.calls.lock().unwrap().push((text.into(), task));
        if text == "fail" {
            return Err(DbError::vectorizer("injected scalar failure"));
        }
        Ok(ArtifactTextVector {
            engine_id: "scalar".into(),
            model: None,
            dimensions: 1,
            vector: vec![if text == "first" { 1.0 } else { 2.0 }],
            normalized: false,
            metadata: serde_json::json!({}),
        })
    }
}

#[test]
fn default_batch_preserves_order_task_and_empty_input() {
    let fake = NamedScalarVectorizer::default();
    let output = fake
        .vectorize_artifact_texts(&["second", "first"], TextEmbeddingTask::Query)
        .unwrap();
    assert_eq!(
        output.iter().map(|v| v.vector.clone()).collect::<Vec<_>>(),
        vec![vec![2.0], vec![1.0]]
    );
    assert_eq!(
        *fake.calls.lock().unwrap(),
        vec![
            ("second".into(), TextEmbeddingTask::Query),
            ("first".into(), TextEmbeddingTask::Query)
        ]
    );
    assert!(
        fake.vectorize_artifact_texts(&[], TextEmbeddingTask::Document)
            .unwrap()
            .is_empty()
    );
    assert_eq!(fake.calls.lock().unwrap().len(), 2);
}

#[test]
fn default_batch_propagates_failure_without_returning_partial_results() {
    let fake = NamedScalarVectorizer::default();
    assert!(
        fake.vectorize_artifact_texts(&["first", "fail", "second"], TextEmbeddingTask::Document)
            .is_err()
    );
    assert_eq!(
        *fake.calls.lock().unwrap(),
        vec![
            ("first".into(), TextEmbeddingTask::Document),
            ("fail".into(), TextEmbeddingTask::Document)
        ]
    );
}
