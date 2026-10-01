use lumvise_db_core::{
    ArtifactTextVector, LocalPersistence, SemanticArtifact, SemanticElement, SemanticOperation,
    SemanticPersistence, SemanticResult, StoredArtifactTextVector, StoredSemanticElementNameVector,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;

const ROOT: &str = "/embedding-identity";
const LEGACY: &str = "fastembed-local";
const CORRECTED: &str = "fastembed-local::bge-cls-v1";

struct VectorIdentityFixture(LocalPersistence);

impl VectorIdentityFixture {
    fn new() -> Self {
        let fixture = Self(LocalPersistence::in_memory().unwrap());
        fixture.execute(SemanticOperation::SyncStructure {
            project_root: ROOT.into(),
            elements: vec![semantic_owner()],
            relationships: vec![],
        });
        fixture.execute(SemanticOperation::UpsertArtifact {
            artifact: knowledge_record(),
            media_type: "text/plain".into(),
        });
        fixture
    }

    fn execute(&self, operation: SemanticOperation) -> SemanticResult {
        self.0
            .execute(operation, &InvocationControl::sixty_seconds())
            .unwrap()
    }

    fn store(&self, engine_id: &str) {
        self.execute(SemanticOperation::StoreElementNameVectors {
            project_root: ROOT.into(),
            vectors: vec![StoredSemanticElementNameVector {
                semantic_element_id: "owner".into(),
                project_root: ROOT.into(),
                source_text: "example".into(),
                vector: embedding(engine_id),
            }],
        });
        self.execute(SemanticOperation::StoreArtifactTextVectors {
            project_root: ROOT.into(),
            vectors: vec![StoredArtifactTextVector {
                artifact_id: "knowledge".into(),
                semantic_element_id: "owner".into(),
                source_text: "example".into(),
                vector: embedding(engine_id),
            }],
        });
    }

    fn assert_matches(&self, engine_id: &str, model: &str, expected: usize) {
        for operation in [
            SemanticOperation::SearchElementNameVectors {
                project_root: ROOT.into(),
                query: vec![1.0, 0.0],
                k: 5,
                engine_id: engine_id.into(),
                model: Some(model.into()),
            },
            SemanticOperation::SearchArtifactTextVectors {
                project_root: ROOT.into(),
                query: vec![1.0, 0.0],
                k: 5,
                engine_id: engine_id.into(),
                model: Some(model.into()),
            },
        ] {
            match self.execute(operation) {
                SemanticResult::ElementVectorSearch(found)
                | SemanticResult::ArtifactVectorSearch(found) => {
                    assert_eq!(found.len(), expected, "{engine_id}/{model}: {found:?}");
                }
                unexpected => panic!("expected vector search result, got {unexpected:?}"),
            }
        }
    }
}

fn semantic_owner() -> SemanticElement {
    SemanticElement {
        project_root: ROOT.into(),
        semantic_element_id: "owner".into(),
        semantic_source_id: "fixture".into(),
        path: "example.rs".into(),
        element_kind: "function".into(),
        name: "example".into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: Some(1),
        end_line: Some(2),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn knowledge_record() -> SemanticArtifact {
    SemanticArtifact {
        artifact_id: "knowledge".into(),
        semantic_element_id: "owner".into(),
        artifact_kind: "annotation".into(),
        title: "example".into(),
        content_ref: None,
        content: Some("example".into()),
        searchable_text: Some("example".into()),
        content_size_bytes: None,
        dependencies: vec![],
        metadata: json!({}),
    }
}

fn embedding(engine_id: &str) -> ArtifactTextVector {
    ArtifactTextVector {
        engine_id: engine_id.into(),
        model: Some("bge-small-en-v1.5".into()),
        dimensions: 2,
        vector: vec![1.0, 0.0],
        normalized: true,
        metadata: json!({}),
    }
}

#[test]
fn corrected_embedding_identity_excludes_old_vectors_until_replacement() {
    let fixture = VectorIdentityFixture::new();
    fixture.store(LEGACY);
    fixture.assert_matches(LEGACY, "bge-small-en-v1.5", 1);
    fixture.assert_matches(CORRECTED, "bge-small-en-v1.5", 0);
    fixture.store(CORRECTED);
    fixture.assert_matches(CORRECTED, "bge-small-en-v1.5", 1);
    fixture.assert_matches(LEGACY, "bge-small-en-v1.5", 0);
    fixture.assert_matches(CORRECTED, "bge-m3", 0);
}
