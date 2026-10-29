use lumvise_db_core::{
    ArtifactBlob, ArtifactDependency, ArtifactDependencyTarget, ArtifactTextVector,
    CentralizedPersistence, DbError, LocalPersistence, PzFailurePhase, SemanticArchive,
    SemanticArtifact, SemanticElement, SemanticOperation, SemanticPersistence,
    SemanticProjectSnapshot, SemanticRelationship, SemanticResult, StoredArtifactTextVector,
    StoredSemanticElementNameVector,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;

fn pz_contents() -> SemanticArchive {
    SemanticArchive {
        display_name: Some("Origin name".into()),
        source_revision: Some("origin-commit".into()),
        selection_id: "working".into(),
        revision_state: None,
        project_id: uuid::Uuid::new_v4().to_string(),
        snapshot_id: uuid::Uuid::now_v7().to_string(),
        canonical_remote: Some("https://example.test/project.git".into()),
        snapshot: SemanticProjectSnapshot {
            commit_version: 17,
            published_at: "2026-01-02T03:04:05+00:00".into(),
            project_root: "/source".into(),
            elements: vec![pz_element("a"), pz_element("b")],
            relationships: vec![SemanticRelationship {
                project_root: "/source".into(),
                source_element_id: "a".into(),
                target_element_id: "b".into(),
                relationship_kind: "calls".into(),
                label: "calls".into(),
                metadata: json!({"count":1}),
            }],
            artifacts: vec![SemanticArtifact {
                artifact_id: "guide".into(),
                semantic_element_id: "a".into(),
                artifact_kind: "definition".into(),
                title: "Guide".into(),
                content_ref: Some("blob:guide".into()),
                content: None,
                searchable_text: Some("Guide text".into()),
                content_size_bytes: Some(4),
                dependencies: vec![],
                metadata: json!({"tag":"portable"}),
            }],
        },
        artifact_blobs: vec![ArtifactBlob {
            content_ref: "blob:guide".into(),
            artifact_id: "guide".into(),
            media_type: "application/octet-stream".into(),
            content: vec![0, 1, 254, 255],
            updated_at: "2026-01-02T03:04:05+00:00".into(),
        }],
        artifact_text_vectors: vec![StoredArtifactTextVector {
            artifact_id: "guide".into(),
            semantic_element_id: "a".into(),
            source_text: "Guide text".into(),
            vector: pz_vector(),
        }],
        element_name_vectors: vec![StoredSemanticElementNameVector {
            semantic_element_id: "a".into(),
            project_root: "/source".into(),
            source_text: "a".into(),
            vector: pz_vector(),
        }],
    }
}
fn pz_element(id: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/source".into(),
        semantic_element_id: id.into(),
        semantic_source_id: "source".into(),
        path: format!("src/{id}.rs"),
        element_kind: "file".into(),
        name: id.into(),
        parent_element_id: None,
        revision_dependencies: if id == "b" {
            vec![lumvise_db_core::SemanticRecordRef::artifact("guide")]
        } else {
            vec![]
        },
        content_fingerprint: Some("fp1:0000000000001234:content".into()),
        start_line: Some(1),
        end_line: Some(2),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({"id":id}),
    }
}
fn pz_vector() -> ArtifactTextVector {
    ArtifactTextVector {
        engine_id: "engine".into(),
        model: Some("model".into()),
        dimensions: 2,
        vector: vec![0.6, 0.8],
        normalized: true,
        metadata: json!({"revision":1}),
    }
}
fn pz_phase(error: DbError, expected: PzFailurePhase) {
    assert!(matches!(error, DbError::Pz { phase, .. } if phase==expected));
}

fn seeded_archive_source() -> (LocalPersistence, SemanticArchive) {
    let persistence = LocalPersistence::in_memory().unwrap();
    let mut source = pz_contents();
    for element in &mut source.snapshot.elements {
        element.content_fingerprint = None;
    }
    source.snapshot.artifacts[0].dependencies = vec![ArtifactDependency {
        target: ArtifactDependencyTarget::SemanticElement {
            semantic_element_id: "b".into(),
        },
    }];
    source.snapshot.artifacts[0].metadata = json!({"nul": "a\0b", "integer": i64::MAX});
    let mut dependent = source.snapshot.artifacts[0].clone();
    dependent.artifact_id = "dependent".into();
    dependent.content_ref = None;
    dependent.dependencies = vec![ArtifactDependency {
        target: ArtifactDependencyTarget::Artifact {
            edit_version: None,
            artifact_id: "guide".into(),
        },
    }];
    source.snapshot.artifacts.push(dependent);
    seed_archive_records(&persistence, &source);
    (persistence, source)
}

fn archive_execute(persistence: &LocalPersistence, operation: SemanticOperation) -> SemanticResult {
    SemanticPersistence::execute(persistence, operation, &InvocationControl::sixty_seconds())
        .unwrap()
}

fn seed_archive_records(persistence: &LocalPersistence, source: &SemanticArchive) {
    archive_execute(
        persistence,
        SemanticOperation::SyncStructure {
            project_root: "/source".into(),
            elements: source.snapshot.elements.clone(),
            relationships: source.snapshot.relationships.clone(),
        },
    );
    for artifact in &source.snapshot.artifacts {
        archive_execute(
            persistence,
            SemanticOperation::UpsertArtifact {
                artifact: artifact.clone(),
                media_type: "application/octet-stream".into(),
            },
        );
    }
    for blob in &source.artifact_blobs {
        archive_execute(
            persistence,
            SemanticOperation::ArtifactBlobPut {
                content_ref: blob.content_ref.clone(),
                artifact_id: blob.artifact_id.clone(),
                media_type: blob.media_type.clone(),
                content: blob.content.clone(),
            },
        );
    }
    archive_execute(
        persistence,
        SemanticOperation::StoreElementNameVectors {
            project_root: "/source".into(),
            vectors: source.element_name_vectors.clone(),
        },
    );
    archive_execute(
        persistence,
        SemanticOperation::StoreArtifactTextVectors {
            project_root: "/source".into(),
            vectors: source.artifact_text_vectors.clone(),
        },
    );
}

#[test]
fn project_archive_capture_preserves_dependencies_and_all_owned_records_without_pz() {
    let (persistence, source) = seeded_archive_source();
    let SemanticResult::ProjectArchive(captured) = archive_execute(
        &persistence,
        SemanticOperation::ProjectArchive {
            project_root: "/source".into(),
        },
    ) else {
        panic!("expected complete project archive")
    };
    assert_eq!(captured.snapshot.elements, source.snapshot.elements);
    assert_eq!(
        captured.snapshot.relationships,
        source.snapshot.relationships
    );
    for expected in &source.snapshot.artifacts {
        let actual = captured
            .snapshot
            .artifacts
            .iter()
            .find(|artifact| artifact.artifact_id == expected.artifact_id)
            .unwrap();
        assert_eq!(actual.dependencies, expected.dependencies);
        assert_eq!(actual.metadata, expected.metadata);
    }
    assert_eq!(
        captured.artifact_blobs[0].content,
        source.artifact_blobs[0].content
    );
    assert_eq!(captured.element_name_vectors, source.element_name_vectors);
    assert_eq!(captured.artifact_text_vectors, source.artifact_text_vectors);
    let encoded = CentralizedPersistence::encode_semantic_result(SemanticResult::ProjectArchive(
        captured.clone(),
    ))
    .unwrap();
    let SemanticResult::ProjectArchive(decoded) =
        CentralizedPersistence::decode_semantic_result(&encoded).unwrap()
    else {
        panic!("expected portable project archive")
    };
    assert_eq!(decoded, captured);
}

#[test]
fn project_archive_operation_round_trips_and_rejects_expired_or_cancelled_captures() {
    let operation = SemanticOperation::ProjectArchive {
        project_root: "/source".into(),
    };
    let encoded = CentralizedPersistence::encode_semantic_operation(operation.clone()).unwrap();
    assert!(
        matches!(CentralizedPersistence::decode_semantic_operation(&encoded).unwrap(), SemanticOperation::ProjectArchive { project_root } if project_root == "/source")
    );
    let persistence = LocalPersistence::in_memory().unwrap();
    let expired = InvocationControl::with_deadline(std::time::Duration::ZERO);
    assert!(SemanticPersistence::execute(&persistence, operation.clone(), &expired).is_err());
    let cancelled = InvocationControl::sixty_seconds();
    cancelled.cancel();
    assert!(SemanticPersistence::execute(&persistence, operation, &cancelled).is_err());
}

#[test]
fn pz_semantic_archive_round_trip_retains_owned_metadata_blobs_vectors_and_root_remap() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first.pz");
    let second = directory.path().join("second.pz");
    let control = InvocationControl::sixty_seconds();
    let archive = pz_contents();
    let result = archive.write(&first, &control).unwrap();
    assert_eq!(result.project_id, archive.project_id);
    assert_eq!(result.snapshot_id, archive.snapshot_id);
    assert_eq!(result.commit_version, 17);
    assert_eq!(result.published_at, archive.snapshot.published_at);
    assert_eq!(result.row_counts["artifact_blobs.parquet"], 1);
    assert_eq!(result.row_counts["artifact_text_vectors.parquet"], 1);
    assert_eq!(result.row_counts["element_name_vectors.parquet"], 1);
    let decoded = SemanticArchive::read(&first, " /target/ ", &control).unwrap();
    assert_eq!(decoded.snapshot.project_root, "/target");
    assert!(
        decoded
            .snapshot
            .elements
            .iter()
            .all(|element| element.project_root == "/target")
    );
    assert!(
        decoded
            .snapshot
            .relationships
            .iter()
            .all(|relationship| relationship.project_root == "/target")
    );
    assert!(
        decoded
            .element_name_vectors
            .iter()
            .all(|vector| vector.project_root == "/target")
    );
    let mut expected = archive.clone();
    expected.snapshot.project_root = "/target".into();
    for element in &mut expected.snapshot.elements {
        element.project_root = "/target".into();
    }
    for relationship in &mut expected.snapshot.relationships {
        relationship.project_root = "/target".into();
    }
    for vector in &mut expected.element_name_vectors {
        vector.project_root = "/target".into();
    }
    let timestamp = chrono::DateTime::parse_from_rfc3339(&archive.snapshot.published_at)
        .unwrap()
        .timestamp_millis();
    let source_revisions = lumvise_db_core::SemanticRevisionState::adopt(
        "/source",
        &archive.project_id,
        "working",
        "local",
        archive.snapshot.elements.clone(),
        archive.snapshot.artifacts.clone(),
        archive.snapshot.relationships.clone(),
        17,
        timestamp,
    )
    .unwrap();
    expected.revision_state = Some(
        lumvise_db_core::SemanticRevisionState::from_metadata_rows(
            "/target",
            17,
            source_revisions.metadata_rows(),
            expected.snapshot.elements.clone(),
            expected.snapshot.artifacts.clone(),
        )
        .unwrap(),
    );
    assert_eq!(decoded, expected);
    decoded.write(&second, &control).unwrap();
    assert!(
        std::fs::read(first).unwrap() == std::fs::read(second).unwrap(),
        "root rebinding must preserve canonical archive bytes"
    );
}

#[test]
fn pz_semantic_archive_rejects_corrupt_archive() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("broken.pz");
    std::fs::write(&path, b"not a ZIP64 archive").unwrap();
    pz_phase(
        SemanticArchive::read(path, "/target", &InvocationControl::sixty_seconds()).unwrap_err(),
        PzFailurePhase::Validate,
    );
}

#[test]
fn pz_writer_rejects_inconsistent_revision_identity_before_replacing_a_valid_archive() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("archive.pz");
    let control = InvocationControl::sixty_seconds();
    pz_contents().write(&path, &control).unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut archive = SemanticArchive::read(&path, "/source", &control).unwrap();
    archive.revision_state.as_mut().unwrap().records[0]
        .record
        .record_id = "mismatched-id".into();
    pz_phase(
        archive.write(&path, &control).unwrap_err(),
        PzFailurePhase::Encode,
    );
    assert!(std::fs::read(&path).unwrap() == before);
}

fn rewrite_manifest_identity(input: &std::path::Path, output: &std::path::Path) {
    use std::io::{Read, Write};
    let mut archive = zip::ZipArchive::new(std::fs::File::open(input).unwrap()).unwrap();
    let mut writer = zip::ZipWriter::new(std::fs::File::create(output).unwrap());
    writer.set_zip64_comment(Some("PZ2"));
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let mut bytes = vec![];
        entry.read_to_end(&mut bytes).unwrap();
        if entry.name() == "manifest.json" {
            let mut manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            manifest["projectId"] = json!(uuid::Uuid::new_v4().to_string());
            bytes = serde_json::to_vec(&manifest).unwrap();
        }
        writer
            .start_file(
                entry.name(),
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored)
                    .large_file(true),
            )
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn archive_project_identity_must_match_revision_metadata_on_write_and_read() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("original.pz");
    let altered = directory.path().join("another-project.pz");
    let control = InvocationControl::sixty_seconds();
    pz_contents().write(&path, &control).unwrap();
    let original = std::fs::read(&path).unwrap();
    let mut archive = SemanticArchive::read(&path, "/source", &control).unwrap();
    archive.revision_state.as_mut().unwrap().project_id = uuid::Uuid::new_v4().to_string();
    let error = archive.write(&path, &control).unwrap_err();
    assert!(error.to_string().contains("archive project identity"));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    rewrite_manifest_identity(&path, &altered);
    let error = SemanticArchive::read(&altered, "/source", &control).unwrap_err();
    assert!(error.to_string().contains("archive project identity"));
}

#[test]
fn pz_writer_requires_unique_exact_blob_references_for_retained_bodies() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("archive.pz");
    let control = InvocationControl::sixty_seconds();
    let mut archive = pz_contents();
    archive.write(&path, &control).unwrap();
    let before = std::fs::read(&path).unwrap();
    archive
        .artifact_blobs
        .push(archive.artifact_blobs[0].clone());
    pz_phase(
        archive.write(&path, &control).unwrap_err(),
        PzFailurePhase::Encode,
    );
    archive.artifact_blobs.clear();
    pz_phase(
        archive.write(&path, &control).unwrap_err(),
        PzFailurePhase::Encode,
    );
    assert!(std::fs::read(&path).unwrap() == before);
}

#[test]
fn pz_semantic_archive_cancelled_read_and_write_preserve_existing_output() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.pz");
    let archive = pz_contents();
    archive
        .write(&path, &InvocationControl::sixty_seconds())
        .unwrap();
    let existing = std::fs::read(&path).unwrap();
    let cancelled = InvocationControl::sixty_seconds();
    cancelled.cancel();
    pz_phase(
        archive.write(&path, &cancelled).unwrap_err(),
        PzFailurePhase::Cancellation,
    );
    pz_phase(
        SemanticArchive::read(&path, "/target", &cancelled).unwrap_err(),
        PzFailurePhase::Cancellation,
    );
    assert_eq!(std::fs::read(path).unwrap(), existing);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn pz_semantic_archive_invalid_public_identity_does_not_publish() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.pz");
    let mut archive = pz_contents();
    archive.snapshot_id = "../invalid".into();
    pz_phase(
        archive
            .write(path, &InvocationControl::sixty_seconds())
            .unwrap_err(),
        PzFailurePhase::Validate,
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn pz_semantic_archive_retains_existing_external_reference_exclusion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("external.pz");
    let mut archive = pz_contents();
    archive.snapshot.relationships.push(SemanticRelationship {
        project_root: "/source".into(),
        source_element_id: "a".into(),
        target_element_id: "foreign".into(),
        relationship_kind: "uses".into(),
        label: "uses".into(),
        metadata: json!({"foreign_project_id":uuid::Uuid::new_v4().to_string()}),
    });
    let control = InvocationControl::sixty_seconds();
    let written = archive.write(&path, &control).unwrap();
    assert_eq!(written.row_counts["external_references.parquet"], 1);
    let decoded = SemanticArchive::read(path, "/target", &control).unwrap();
    assert_eq!(decoded.snapshot.relationships.len(), 1);
    assert_eq!(decoded.snapshot.relationships[0].target_element_id, "b");
}
