use lumvise_db_core::{
    ArtifactDependency, ArtifactDependencyTarget, ArtifactTextVector, LocalPersistence,
    SemanticArtifact, SemanticElement, SemanticOperation, SemanticPersistence, SemanticResult,
    StoredArtifactTextVector, StoredSemanticElementNameVector,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};
use std::collections::HashSet;

const ROOT: &str = "/sync-attachments";
const ATTACHMENT: &str = "canvas-file:note:image";

struct StructuralAttachments {
    persistence: LocalPersistence,
    elements: Vec<SemanticElement>,
    content_ref: String,
}
impl StructuralAttachments {
    fn seed(persistence: LocalPersistence) -> Self {
        let elements = (0..600).map(element).collect::<Vec<_>>();
        execute(&persistence, sync(elements.clone()));
        execute(
            &persistence,
            SemanticOperation::UpsertArtifact {
                artifact: note(),
                media_type: "text/plain".into(),
            },
        );
        let SemanticResult::Artifact(Some(note)) = execute(
            &persistence,
            SemanticOperation::Artifact {
                artifact_id: "note".into(),
            },
        ) else {
            panic!("artifact expected")
        };
        let artifact_source = note
            .searchable_text
            .as_deref()
            .or(note.content.as_deref())
            .unwrap()
            .trim()
            .to_owned();
        let content_ref = note
            .content_ref
            .expect("large content spills to owned SQL blob");
        store_vectors(&persistence, &artifact_source);
        execute(
            &persistence,
            SemanticOperation::ArtifactBlobPut {
                content_ref: ATTACHMENT.into(),
                artifact_id: "note".into(),
                media_type: "image/png".into(),
                content: vec![0, 255, 17],
            },
        );
        Self {
            persistence,
            elements,
            content_ref,
        }
    }
    fn replace(&mut self, changes: usize, remove: bool) {
        for element in self.elements.iter_mut().take(changes) {
            element.name.push_str(" changed");
        }
        if remove {
            self.elements
                .retain(|element| element.semantic_element_id != "e598");
        }
        execute(&self.persistence, sync(self.elements.clone()));
    }
    fn assert_retained(&self) {
        for (operation, expected_id) in vector_queries() {
            let result = execute(&self.persistence, operation);
            let vectors = match result {
                SemanticResult::ElementVectorSearch(vectors)
                | SemanticResult::ArtifactVectorSearch(vectors) => vectors,
                _ => panic!("vector search expected"),
            };
            assert_eq!(vectors.len(), 1);
            assert_eq!(vectors[0].id, expected_id);
        }
        let SemanticResult::Artifacts(dependents) = execute(
            &self.persistence,
            SemanticOperation::ArtifactDependents {
                target_kind: "semantic_element".into(),
                target_id: "e599".into(),
            },
        ) else {
            panic!("dependents expected")
        };
        assert_eq!(dependents.len(), 1);
        assert_eq!(dependents[0].artifact_id, "note");
        let SemanticResult::Artifacts(owned) = execute(
            &self.persistence,
            SemanticOperation::ArtifactsForElements {
                semantic_element_ids: HashSet::from(["e599".into()]),
            },
        ) else {
            panic!("owned artifacts expected")
        };
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].content.as_deref(), Some(note_content().as_str()));
        for (content_ref, expected) in [
            (&self.content_ref, note_content().into_bytes()),
            (&ATTACHMENT.to_owned(), vec![0, 255, 17]),
        ] {
            let SemanticResult::ArtifactBlob(Some(blob)) = execute(
                &self.persistence,
                SemanticOperation::ArtifactBlobGet {
                    content_ref: content_ref.clone(),
                },
            ) else {
                panic!("owned blob expected")
            };
            assert_eq!(blob.content, expected);
        }
    }
    fn observed(&self) -> Vec<Value> {
        let operations = vector_queries()
            .map(|(operation, _)| operation)
            .into_iter()
            .chain([
                SemanticOperation::Artifact {
                    artifact_id: "note".into(),
                },
                SemanticOperation::ArtifactDependents {
                    target_kind: "semantic_element".into(),
                    target_id: "e599".into(),
                },
                SemanticOperation::ElementsByIdsIncludingInactive {
                    project_root: ROOT.into(),
                    semantic_element_ids: self
                        .elements
                        .iter()
                        .map(|element| element.semantic_element_id.clone())
                        .collect(),
                },
                SemanticOperation::SemanticRevision,
            ]);
        operations
            .map(|operation| serde_json::to_value(execute(&self.persistence, operation)).unwrap())
            .collect()
    }
}
fn execute(persistence: &LocalPersistence, operation: SemanticOperation) -> SemanticResult {
    persistence
        .execute(operation, &InvocationControl::sixty_seconds())
        .unwrap()
}
fn sync(elements: Vec<SemanticElement>) -> SemanticOperation {
    SemanticOperation::SyncStructure {
        project_root: ROOT.into(),
        elements,
        relationships: vec![],
    }
}
fn element(index: usize) -> SemanticElement {
    SemanticElement {
        project_root: ROOT.into(),
        semantic_element_id: format!("e{index}"),
        semantic_source_id: "fixture".into(),
        path: format!("src/e{index}.rs"),
        element_kind: "file".into(),
        name: format!("E{index}"),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}
fn note_content() -> String {
    "large artifact payload ".repeat(4096)
}
fn note() -> SemanticArtifact {
    SemanticArtifact {
        artifact_id: "note".into(),
        semantic_element_id: "e599".into(),
        artifact_kind: "note".into(),
        title: "Note".into(),
        content_ref: None,
        content: Some(note_content()),
        searchable_text: Some("Searchable note".into()),
        content_size_bytes: None,
        dependencies: vec![ArtifactDependency {
            target: ArtifactDependencyTarget::SemanticElement {
                semantic_element_id: "e599".into(),
            },
        }],
        metadata: json!({}),
    }
}
fn vector() -> ArtifactTextVector {
    ArtifactTextVector {
        engine_id: "regression".into(),
        model: None,
        dimensions: 2,
        normalized: false,
        vector: vec![1.0, 0.0],
        metadata: json!({}),
    }
}
fn store_vectors(persistence: &LocalPersistence, artifact_source: &str) {
    execute(
        persistence,
        SemanticOperation::StoreElementNameVectors {
            project_root: ROOT.into(),
            vectors: vec![StoredSemanticElementNameVector {
                semantic_element_id: "e599".into(),
                project_root: ROOT.into(),
                source_text: "E599".into(),
                vector: vector(),
            }],
        },
    );
    execute(
        persistence,
        SemanticOperation::StoreArtifactTextVectors {
            project_root: ROOT.into(),
            vectors: vec![StoredArtifactTextVector {
                artifact_id: "note".into(),
                semantic_element_id: "e599".into(),
                source_text: artifact_source.into(),
                vector: vector(),
            }],
        },
    );
}
fn vector_queries() -> [(SemanticOperation, &'static str); 2] {
    [
        (
            SemanticOperation::SearchElementNameVectors {
                project_root: ROOT.into(),
                query: vec![1.0, 0.0],
                k: 1,
                engine_id: "regression".into(),
                model: None,
            },
            "e599",
        ),
        (
            SemanticOperation::SearchArtifactTextVectors {
                project_root: ROOT.into(),
                query: vec![1.0, 0.0],
                k: 1,
                engine_id: "regression".into(),
                model: None,
            },
            "note",
        ),
    ]
}
#[test]
fn structural_sync_keeps_owned_attachments_on_both_sides_of_rebuild_threshold() {
    for changes in [512, 513] {
        let mut fixture = StructuralAttachments::seed(LocalPersistence::in_memory().unwrap());
        fixture.replace(changes, false);
        fixture.assert_retained();
    }
}
#[test]
fn structural_sync_retains_attachments_and_removed_tombstones_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("semantic.db");
    let mut fixture = StructuralAttachments::seed(LocalPersistence::open(&path).unwrap());
    fixture.replace(513, true);
    fixture.assert_retained();
    let content_ref = fixture.content_ref.clone();
    let elements = fixture.elements.clone();
    drop(fixture);
    let fixture = StructuralAttachments {
        persistence: LocalPersistence::open(&path).unwrap(),
        elements,
        content_ref,
    };
    fixture.assert_retained();
    let SemanticResult::Elements(elements) = execute(
        &fixture.persistence,
        SemanticOperation::ElementsByIdsIncludingInactive {
            project_root: ROOT.into(),
            semantic_element_ids: HashSet::from(["e598".into()]),
        },
    ) else {
        panic!("tombstone expected")
    };
    assert_eq!(elements.len(), 1);
    assert_eq!(elements[0].lifecycle, "inactive");
}
#[test]
fn invalid_or_cancelled_large_sync_preserves_existing_structure_and_attachments() {
    let fixture = StructuralAttachments::seed(LocalPersistence::in_memory().unwrap());
    let before = fixture.observed();
    let mut incoming = fixture.elements.clone();
    for element in incoming.iter_mut().take(513) {
        element.name.push_str(" changed");
    }
    incoming[598].name.clear();
    assert!(
        fixture
            .persistence
            .execute(sync(incoming), &InvocationControl::sixty_seconds())
            .is_err()
    );
    assert_eq!(fixture.observed(), before);
    let control = InvocationControl::sixty_seconds();
    control.cancel();
    assert!(fixture.persistence.execute(sync(vec![]), &control).is_err());
    assert_eq!(fixture.observed(), before);
    fixture.assert_retained();
}
