use lumvise_db_core::{
    ArtifactTextVector, ArtifactTextVectorizer, DbCore, Result, SemanticArtifact, SemanticElement,
};
use serde_json::json;
use std::sync::Arc;
use std::sync::mpsc;

#[test]
fn text_artifact_upsert_with_vectorizer_stores_embedding() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let vectorizer = NamedFakeArtifactVectorizer::new("fake-vectorizer");
    storage.upsert_element(&element("root")).unwrap();

    storage
        .upsert_artifact_with_vectorizer(&artifact("note", "root", "alpha text"), &vectorizer)
        .unwrap();

    let stored = storage.artifact_text_vector("note").unwrap().unwrap();
    assert_eq!(stored.artifact_id, "note");
    assert_eq!(stored.semantic_element_id, "root");
    assert_eq!(stored.source_text, "alpha text");
    assert_eq!(stored.vector.vector, vec![10.0, 2.0]);
    assert_eq!(stored.vector.dimensions, 2);
    assert_eq!(stored.vector.engine_id, "fake-vectorizer");
}

#[test]
fn text_artifact_update_refreshes_embedding() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let vectorizer = NamedFakeArtifactVectorizer::new("fake-vectorizer");
    storage.upsert_element(&element("root")).unwrap();

    storage
        .upsert_artifact_with_vectorizer(&artifact("note", "root", "alpha"), &vectorizer)
        .unwrap();
    storage
        .upsert_artifact_with_vectorizer(&artifact("note", "root", "updated beta"), &vectorizer)
        .unwrap();

    let stored = storage.artifact_text_vector("note").unwrap().unwrap();
    assert_eq!(stored.source_text, "updated beta");
    assert_eq!(stored.vector.vector, vec![12.0, 2.0]);
}

#[test]
fn artifact_removal_deletes_embedding() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let vectorizer = NamedFakeArtifactVectorizer::new("fake-vectorizer");
    storage.upsert_element(&element("root")).unwrap();
    storage
        .upsert_artifact_with_vectorizer(&artifact("note", "root", "alpha"), &vectorizer)
        .unwrap();

    storage.remove_artifact("note").unwrap();

    assert!(storage.artifact_text_vector("note").unwrap().is_none());
}

#[test]
fn artifact_content_upsert_with_vectorizer_embeds_searchable_text() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let vectorizer = NamedFakeArtifactVectorizer::new("fake-vectorizer");
    storage.upsert_element(&element("root")).unwrap();

    storage
        .upsert_artifact_content_with_vectorizer(
            &artifact("note", "root", ""),
            "text/plain",
            b"content body",
            &vectorizer,
        )
        .unwrap();

    let stored = storage.artifact_text_vector("note").unwrap().unwrap();
    assert_eq!(stored.source_text, "content body");
    assert_eq!(stored.vector.vector, vec![12.0, 2.0]);
}

#[test]
fn new_text_artifact_lazily_creates_embedding() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let vectorizer = NamedFakeArtifactVectorizer::new("fake-vectorizer");
    storage.upsert_element(&element("root")).unwrap();

    storage
        .upsert_artifact(&artifact("note", "root", "lazy text"))
        .unwrap();

    assert!(storage.artifact_text_vector("note").unwrap().is_none());
    let stored = storage
        .ensure_artifact_text_vector("note", &vectorizer)
        .unwrap()
        .unwrap();
    assert_eq!(stored.source_text, "lazy text");
    assert_eq!(stored.vector.vector, vec![9.0, 2.0]);
}

#[test]
fn updated_text_artifact_lazily_refreshes_embedding() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let vectorizer = NamedFakeArtifactVectorizer::new("fake-vectorizer");
    storage.upsert_element(&element("root")).unwrap();

    storage
        .upsert_artifact_with_vectorizer(&artifact("note", "root", "old"), &vectorizer)
        .unwrap();
    storage
        .upsert_artifact(&artifact("note", "root", "updated lazy text"))
        .unwrap();

    assert!(storage.artifact_text_vector("note").unwrap().is_none());
    let stored = storage
        .ensure_artifact_text_vector("note", &vectorizer)
        .unwrap()
        .unwrap();
    assert_eq!(stored.source_text, "updated lazy text");
    assert_eq!(stored.vector.vector, vec![17.0, 3.0]);
}

#[test]
fn failing_vectorizer_does_not_publish_artifact_update_or_vector() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let vectorizer = NamedFakeArtifactVectorizer::new("fake-vectorizer");
    storage.upsert_element(&element("root")).unwrap();
    storage
        .upsert_artifact_with_vectorizer(&artifact("note", "root", "old text"), &vectorizer)
        .unwrap();

    let error = storage
        .upsert_artifact_with_vectorizer(
            &artifact("note", "root", "bad text"),
            &FailingArtifactVectorizer,
        )
        .unwrap_err();

    let artifact = storage.artifact("note").unwrap().unwrap();
    let vector = storage.artifact_text_vector("note").unwrap().unwrap();
    assert!(error.to_string().contains("vectorizer failed"));
    assert_eq!(artifact.searchable_text.as_deref(), Some("old text"));
    assert_eq!(vector.source_text, "old text");
}

#[test]
fn vectorized_artifact_update_is_not_visible_until_vectorizer_finishes() {
    let db = Arc::new(DbCore::in_memory().unwrap());
    let storage = db.storage_manager().semantic_storage();
    let vectorizer = NamedFakeArtifactVectorizer::new("fake-vectorizer");
    storage.upsert_element(&element("root")).unwrap();
    storage
        .upsert_artifact_with_vectorizer(&artifact("note", "root", "old text"), &vectorizer)
        .unwrap();

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer_db = Arc::clone(&db);
    let writer = std::thread::spawn(move || {
        let storage = writer_db.storage_manager().semantic_storage();
        let vectorizer = BlockingArtifactVectorizer::new(entered_tx, release_rx);
        storage
            .upsert_artifact_with_vectorizer(&artifact("note", "root", "updated text"), &vectorizer)
            .unwrap();
    });

    entered_rx.recv().unwrap();
    let visible = db
        .storage_manager()
        .semantic_storage()
        .artifact("note")
        .unwrap()
        .unwrap();
    assert_eq!(visible.searchable_text.as_deref(), Some("old text"));

    release_tx.send(()).unwrap();
    writer.join().unwrap();
    let updated = db
        .storage_manager()
        .semantic_storage()
        .artifact("note")
        .unwrap()
        .unwrap();
    assert_eq!(updated.searchable_text.as_deref(), Some("updated text"));
}

struct NamedFakeArtifactVectorizer {
    engine_id: String,
}

impl NamedFakeArtifactVectorizer {
    fn new(engine_id: &str) -> Self {
        Self {
            engine_id: engine_id.to_string(),
        }
    }
}

impl ArtifactTextVectorizer for NamedFakeArtifactVectorizer {
    fn vectorize_artifact_text(&self, text: &str) -> Result<ArtifactTextVector> {
        Ok(ArtifactTextVector {
            engine_id: self.engine_id.clone(),
            model: Some("fake-model".to_string()),
            dimensions: 2,
            vector: vec![text.len() as f32, text.split_whitespace().count() as f32],
            normalized: false,
            metadata: json!({ "input": text }),
        })
    }
}

struct FailingArtifactVectorizer;

impl ArtifactTextVectorizer for FailingArtifactVectorizer {
    fn vectorize_artifact_text(&self, _text: &str) -> Result<ArtifactTextVector> {
        Err(lumvise_db_core::DbError::vectorizer("vectorizer failed"))
    }
}

struct BlockingArtifactVectorizer {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl BlockingArtifactVectorizer {
    fn new(entered: mpsc::Sender<()>, release: mpsc::Receiver<()>) -> Self {
        Self { entered, release }
    }
}

impl ArtifactTextVectorizer for BlockingArtifactVectorizer {
    fn vectorize_artifact_text(&self, text: &str) -> Result<ArtifactTextVector> {
        self.entered.send(()).unwrap();
        self.release.recv().unwrap();
        Ok(ArtifactTextVector {
            engine_id: "blocking-vectorizer".to_string(),
            model: Some("fake-model".to_string()),
            dimensions: 2,
            vector: vec![text.len() as f32, 1.0],
            normalized: false,
            metadata: json!({ "input": text }),
        })
    }
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
        metadata: json!({}),
        dependencies: vec![],
    }
}
