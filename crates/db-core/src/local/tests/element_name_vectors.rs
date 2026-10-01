use lumvise_db_core::{
    ArtifactTextVector, ArtifactTextVectorizer, DbCore, Result, SemanticArtifact, SemanticElement,
};
use serde_json::json;

#[test]
fn element_upsert_with_vectorizer_stores_name_embedding() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let vectorizer = NamedElementVectorizer::new("engine-a");

    storage
        .upsert_element_with_name_vector(&element("fn-run", "run handler"), &vectorizer)
        .unwrap();

    let stored = storage.element_name_vector("fn-run").unwrap().unwrap();
    assert_eq!(stored.semantic_element_id, "fn-run");
    assert_eq!(stored.project_root, "/repo");
    assert_eq!(stored.source_text, "run handler");
    assert_eq!(stored.vector.engine_id, "engine-a");
    assert_eq!(stored.vector.vector, vec![11.0, 2.0]);
}

#[test]
fn lazy_element_name_vector_refreshes_after_name_change() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let vectorizer = NamedElementVectorizer::new("engine-a");
    storage
        .upsert_element_with_name_vector(&element("fn-run", "run handler"), &vectorizer)
        .unwrap();

    storage
        .upsert_element(&element("fn-run", "execute command"))
        .unwrap();
    let refreshed = storage
        .ensure_element_name_vector("fn-run", &vectorizer)
        .unwrap()
        .unwrap();

    assert_eq!(refreshed.source_text, "execute command");
    assert_eq!(refreshed.vector.vector, vec![15.0, 2.0]);
}

#[test]
fn element_name_vector_lookup_ignores_decoy_node_sharing_property_value() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let vectorizer = NamedElementVectorizer::new("engine-a");

    // A SemanticArtifact node carrying the same semantic_element_id as the target element,
    // indexed on the same property but under a different label.
    storage
        .upsert_element(&element("shared-id", "run handler"))
        .unwrap();
    storage
        .upsert_artifact(&decoy_artifact("shared-id"))
        .unwrap();
    storage
        .upsert_element_with_name_vector(&element("shared-id", "run handler"), &vectorizer)
        .unwrap();

    let stored = storage.element_name_vector("shared-id").unwrap().unwrap();
    assert_eq!(stored.semantic_element_id, "shared-id");
    assert_eq!(stored.source_text, "run handler");
}

struct NamedElementVectorizer {
    engine_id: String,
}

impl NamedElementVectorizer {
    fn new(engine_id: &str) -> Self {
        Self {
            engine_id: engine_id.to_string(),
        }
    }
}

impl ArtifactTextVectorizer for NamedElementVectorizer {
    fn vectorize_artifact_text(&self, text: &str) -> Result<ArtifactTextVector> {
        Ok(ArtifactTextVector {
            engine_id: self.engine_id.clone(),
            model: Some("model-a".to_string()),
            dimensions: 2,
            vector: vec![text.len() as f32, text.split_whitespace().count() as f32],
            normalized: false,
            metadata: json!({ "input": text }),
        })
    }
}

fn decoy_artifact(semantic_element_id: &str) -> SemanticArtifact {
    SemanticArtifact {
        artifact_id: "decoy-artifact".to_string(),
        semantic_element_id: semantic_element_id.to_string(),
        artifact_kind: "note".to_string(),
        title: "decoy".to_string(),
        content_ref: None,
        content: Some("decoy content".to_string()),
        searchable_text: None,
        content_size_bytes: None,
        metadata: json!({}),
        dependencies: vec![],
    }
}

fn element(id: &str, name: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".to_string(),
        semantic_element_id: id.to_string(),
        semantic_source_id: "source".to_string(),
        path: format!("src/{name}.rs"),
        element_kind: "function".to_string(),
        name: name.to_string(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".to_string(),
        match_evidence: None,
        metadata: json!({}),
    }
}
