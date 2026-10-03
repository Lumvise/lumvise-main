use lumvise_db_core::{
    ArtifactBlob, ArtifactTextVector, DbError, PzFailurePhase, SemanticArchive, SemanticArtifact,
    SemanticElement, SemanticProjectSnapshot, SemanticRelationship, StoredArtifactTextVector,
    StoredSemanticElementNameVector,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;

fn pz_contents() -> SemanticArchive {
    SemanticArchive {
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
        content_fingerprint: Some("fp1:1234:content".into()),
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
    assert_eq!(decoded, expected);
    decoded.write(&second, &control).unwrap();
    assert_eq!(
        std::fs::read(first).unwrap(),
        std::fs::read(second).unwrap()
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
