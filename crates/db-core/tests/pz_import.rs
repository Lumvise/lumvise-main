use lumvise_db_core::{
    ArtifactTextVector, DbError, LocalPersistence, PzFailurePhase, SemanticArtifact,
    SemanticElement, SemanticOperation, SemanticPersistence, SemanticRelationship, SemanticResult,
    StoredArtifactTextVector, StoredSemanticElementNameVector,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

#[test]
fn import_round_trips_structure_artifacts_blobs_and_vectors() {
    let directory = tempfile::tempdir().unwrap();
    let archive = directory.path().join("source.pz");
    let source = LocalPersistence::in_memory().unwrap();
    seed_exportable_project(&source);
    let exported = export(&source, &archive);

    let target = LocalPersistence::in_memory().unwrap();
    sync(&target, vec![element("stale", "src/stale.rs")], vec![]);
    let imported = import(&target, &archive).expect("valid archive imports");

    assert_eq!(imported.structure.elements_upserted, 2);
    assert_eq!(imported.structure.elements_marked_inactive, 1);
    assert_eq!(imported.artifacts_imported, 1);
    assert_eq!(imported.artifacts_kept, 0);
    assert_eq!(imported.canonical_remote, None);
    assert_eq!(
        lifecycles(&target),
        BTreeMap::from([
            ("a".to_owned(), "active".to_owned()),
            ("b".to_owned(), "active".to_owned()),
            ("stale".to_owned(), "inactive".to_owned()),
        ])
    );
    let reexported = export(&target, &directory.path().join("target.pz"));
    assert_eq!(reexported, exported);
}

#[test]
fn import_keeps_existing_artifacts_authoritative() {
    let directory = tempfile::tempdir().unwrap();
    let archive = directory.path().join("source.pz");
    let source = LocalPersistence::in_memory().unwrap();
    seed_exportable_project(&source);
    export(&source, &archive);

    let target = LocalPersistence::in_memory().unwrap();
    sync(&target, vec![element("a", "src/a.rs")], vec![]);
    let mut local = artifact("Local edit");
    local.content = Some("kept locally".into());
    upsert(&target, local);
    let imported = import(&target, &archive).expect("valid archive imports");

    assert_eq!(imported.artifacts_imported, 0);
    assert_eq!(imported.artifacts_kept, 1);
    let SemanticResult::Artifact(Some(stored)) = execute(
        &target,
        SemanticOperation::Artifact {
            artifact_id: "guide".into(),
        },
    ) else {
        panic!("expected kept artifact");
    };
    assert_eq!(stored.title, "Local edit");
}

#[test]
fn import_rejects_invalid_archive_without_touching_structure() {
    let directory = tempfile::tempdir().unwrap();
    let archive = directory.path().join("broken.pz");
    std::fs::write(&archive, b"not a pz archive").unwrap();
    let target = LocalPersistence::in_memory().unwrap();
    sync(&target, vec![element("stale", "src/stale.rs")], vec![]);

    let failure = import(&target, &archive).expect_err("invalid archive must fail");

    assert!(matches!(
        failure,
        DbError::Pz {
            phase: PzFailurePhase::Validate,
            ..
        }
    ));
    assert_eq!(
        lifecycles(&target),
        BTreeMap::from([("stale".to_owned(), "active".to_owned())])
    );
}

#[test]
fn import_restores_attachment_blobs_under_original_refs() {
    let directory = tempfile::tempdir().unwrap();
    let archive = directory.path().join("source.pz");
    let source = LocalPersistence::in_memory().unwrap();
    seed_exportable_project(&source);
    put_attachment(&source);
    let exported = export(&source, &archive);
    assert_eq!(exported["artifact_blobs.parquet"], 2);

    let target = LocalPersistence::in_memory().unwrap();
    import(&target, &archive).expect("valid archive imports");

    assert_eq!(
        attachment(&target),
        Some(("image/png".to_owned(), PNG_BYTES.to_vec()))
    );
}

#[test]
fn removing_an_artifact_deletes_its_attachment_blobs() {
    let persistence = LocalPersistence::in_memory().unwrap();
    seed_exportable_project(&persistence);
    put_attachment(&persistence);

    execute(
        &persistence,
        SemanticOperation::RemoveArtifact {
            artifact_id: "guide".into(),
        },
    );

    assert_eq!(attachment(&persistence), None);
}

const ATTACHMENT_REF: &str = "canvas-file:guide:0123abcd";
const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\nimage-bytes";

fn put_attachment(persistence: &LocalPersistence) {
    execute(
        persistence,
        SemanticOperation::ArtifactBlobPut {
            content_ref: ATTACHMENT_REF.into(),
            artifact_id: "guide".into(),
            media_type: "image/png".into(),
            content: PNG_BYTES.to_vec(),
        },
    );
}

fn attachment(persistence: &LocalPersistence) -> Option<(String, Vec<u8>)> {
    let SemanticResult::ArtifactBlob(blob) = execute(
        persistence,
        SemanticOperation::ArtifactBlobGet {
            content_ref: ATTACHMENT_REF.into(),
        },
    ) else {
        panic!("expected artifact blob result");
    };
    blob.map(|blob| (blob.media_type, blob.content))
}

fn seed_exportable_project(persistence: &LocalPersistence) {
    sync(
        persistence,
        vec![element("a", "src/a.rs"), element("b", "src/b.rs")],
        vec![SemanticRelationship {
            project_root: "/repo".into(),
            source_element_id: "a".into(),
            target_element_id: "b".into(),
            relationship_kind: "calls".into(),
            label: "calls".into(),
            metadata: json!({}),
        }],
    );
    let mut guide = artifact("Guide");
    guide.content = Some("hello guide".into());
    upsert(persistence, guide);
    execute(
        persistence,
        SemanticOperation::StoreArtifactTextVectors {
            project_root: "/repo".into(),
            vectors: vec![StoredArtifactTextVector {
                artifact_id: "guide".into(),
                semantic_element_id: "a".into(),
                source_text: "hello guide".into(),
                vector: vector(),
            }],
        },
    );
    execute(
        persistence,
        SemanticOperation::StoreElementNameVectors {
            project_root: "/repo".into(),
            vectors: vec![StoredSemanticElementNameVector {
                semantic_element_id: "a".into(),
                project_root: "/repo".into(),
                source_text: "a".into(),
                vector: vector(),
            }],
        },
    );
}

fn export(persistence: &LocalPersistence, path: &Path) -> BTreeMap<String, u64> {
    let SemanticResult::PzSnapshot(snapshot) = execute(
        persistence,
        SemanticOperation::CreatePzSnapshot {
            project_root: "/repo".into(),
            output_path: path.to_string_lossy().into_owned(),
        },
    ) else {
        panic!("expected PZ snapshot");
    };
    snapshot.row_counts
}

fn import(
    persistence: &LocalPersistence,
    path: &Path,
) -> Result<lumvise_db_core::PzImportResult, DbError> {
    match SemanticPersistence::execute(
        persistence,
        SemanticOperation::ImportPzSnapshot {
            project_root: "/repo".into(),
            input_path: path.to_string_lossy().into_owned(),
        },
        &InvocationControl::sixty_seconds(),
    )? {
        SemanticResult::PzImport(result) => Ok(result),
        other => panic!("expected PZ import result, got {other:?}"),
    }
}

fn lifecycles(persistence: &LocalPersistence) -> BTreeMap<String, String> {
    let SemanticResult::Elements(elements) = execute(
        persistence,
        SemanticOperation::ElementsByIdsIncludingInactive {
            project_root: "/repo".into(),
            semantic_element_ids: HashSet::from(["a".into(), "b".into(), "stale".into()]),
        },
    ) else {
        panic!("expected elements");
    };
    elements
        .into_iter()
        .map(|element| (element.semantic_element_id, element.lifecycle))
        .collect()
}

fn sync(
    persistence: &LocalPersistence,
    elements: Vec<SemanticElement>,
    relationships: Vec<SemanticRelationship>,
) {
    execute(
        persistence,
        SemanticOperation::SyncStructure {
            project_root: "/repo".into(),
            elements,
            relationships,
        },
    );
}

fn upsert(persistence: &LocalPersistence, artifact: SemanticArtifact) {
    execute(
        persistence,
        SemanticOperation::UpsertArtifact {
            artifact,
            media_type: "text/markdown".into(),
        },
    );
}

fn execute(persistence: &LocalPersistence, operation: SemanticOperation) -> SemanticResult {
    SemanticPersistence::execute(persistence, operation, &InvocationControl::sixty_seconds())
        .expect("valid semantic operation")
}

fn element(id: &str, path: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".into(),
        semantic_element_id: id.into(),
        semantic_source_id: "source".into(),
        path: path.into(),
        element_kind: "file".into(),
        name: id.into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: Some(1),
        end_line: Some(2),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({"origin": id}),
    }
}

fn artifact(title: &str) -> SemanticArtifact {
    SemanticArtifact {
        artifact_id: "guide".into(),
        semantic_element_id: "a".into(),
        artifact_kind: "definition".into(),
        title: title.into(),
        content_ref: None,
        content: None,
        searchable_text: None,
        content_size_bytes: None,
        dependencies: vec![],
        metadata: json!({"tags": ["demo"]}),
    }
}

fn vector() -> ArtifactTextVector {
    ArtifactTextVector {
        engine_id: "test-engine".into(),
        model: Some("test-model".into()),
        dimensions: 2,
        vector: vec![0.6, 0.8],
        normalized: true,
        metadata: json!({"revision": 1}),
    }
}
