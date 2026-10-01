use arrow_array::{Array, BinaryArray, Float32Array, ListArray};
use bytes::Bytes;
use lumvise_db_core::{
    ArtifactTextVector, ArtifactTextVectorizer, DbCore, PZ_REQUIRED_ENTRIES, PzArchive, Result,
    SemanticArtifact, SemanticElement, SemanticRelationship, StoredSemanticElementNameVector,
};
use lumvise_resource_routing::InvocationControl;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::json;
use std::fs;
use std::io::{Read, Write};

fn element(id: &str, path: &str, lifecycle: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".into(),
        semantic_element_id: id.into(),
        semantic_source_id: format!("source-{id}"),
        path: path.into(),
        element_kind: "function".into(),
        name: id.into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: Some(1),
        end_line: Some(2),
        lifecycle: lifecycle.into(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn relationship(source: &str, target: &str) -> SemanticRelationship {
    SemanticRelationship {
        project_root: "/repo".into(),
        source_element_id: source.into(),
        target_element_id: target.into(),
        relationship_kind: "calls".into(),
        label: "calls".into(),
        metadata: json!({}),
    }
}

fn artifact(id: &str, owner: &str, text: &str, metadata: serde_json::Value) -> SemanticArtifact {
    SemanticArtifact {
        artifact_id: id.into(),
        semantic_element_id: owner.into(),
        artifact_kind: "note".into(),
        title: id.into(),
        content_ref: None,
        content: None,
        searchable_text: Some(text.into()),
        content_size_bytes: None,
        metadata,
        dependencies: vec![],
    }
}

struct TestVectorizer;

impl ArtifactTextVectorizer for TestVectorizer {
    fn vectorize_artifact_text(&self, _text: &str) -> Result<ArtifactTextVector> {
        Ok(ArtifactTextVector {
            engine_id: "test-engine".into(),
            model: Some("test-model".into()),
            dimensions: 2,
            vector: vec![1.25, -2.5],
            normalized: true,
            metadata: json!({"source": "focused-test", "revision": 7}),
        })
    }
}
fn parquet_entry(path: &std::path::Path, name: &str) -> Vec<u8> {
    let file = fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut entry = archive.by_name(name).unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
    bytes
}

fn rewrite_manifest(path: &std::path::Path, mutate: impl FnOnce(&mut serde_json::Value)) {
    let file = fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut entries = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        entries.push((entry.name().to_owned(), bytes));
    }
    let manifest = entries
        .iter_mut()
        .find(|(name, _)| name == "manifest.json")
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&manifest.1).unwrap();
    mutate(&mut value);
    manifest.1 = serde_json::to_vec(&value).unwrap();

    let replacement = path.with_extension("rewritten.pz");
    let file = fs::File::create(&replacement).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    writer.set_zip64_comment(Some("PZ1"));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(true);
    for (name, bytes) in entries {
        writer.start_file(name, options).unwrap();
        writer.write_all(&bytes).unwrap();
    }
    writer.finish().unwrap();
    fs::rename(replacement, path).unwrap();
}

fn first_batch(bytes: &[u8]) -> arrow_array::RecordBatch {
    let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::copy_from_slice(bytes)).unwrap();
    let mut reader = builder.with_batch_size(1024).build().unwrap();
    reader.next().unwrap().unwrap()
}
fn seeded_db() -> DbCore {
    let db = DbCore::in_memory().unwrap();
    db.storage_manager()
        .semantic_storage()
        .sync_semantic_structure(
            "/repo",
            &[
                element("active-a", "src/a.rs", "active"),
                element("active-b", "src/a.rs", "active"),
                element("inactive", "src/old.rs", "inactive"),
            ],
            &[
                relationship("active-a", "active-b"),
                relationship("active-a", "inactive"),
            ],
        )
        .unwrap();
    db
}

#[test]
fn writes_valid_graph_only_package_and_selective_lookup() {
    let db = seeded_db();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph_db.pz");
    let result = db.create_semantic_snapshot("/repo", &path).unwrap();
    assert_eq!(result.row_counts["elements.parquet"], 2);
    assert_eq!(result.row_counts["relationships.parquet"], 1);
    assert_eq!(result.row_counts["external_references.parquet"], 0);
    let package = fs::read(&path).unwrap();
    assert!(package.windows(4).any(|bytes| bytes == b"PK\x06\x06"));
    assert!(package.windows(4).any(|bytes| bytes == b"PK\x06\x07"));
    for name in PZ_REQUIRED_ENTRIES {
        assert!(
            zip::ZipArchive::new(std::fs::File::open(&path).unwrap())
                .unwrap()
                .by_name(name)
                .is_ok(),
            "missing {name}"
        );
    }
    let archive = PzArchive::open(&path).unwrap();
    assert_eq!(
        archive
            .lookup_path("src/a.rs", false)
            .unwrap()
            .elements
            .len(),
        2
    );
    assert_eq!(
        archive
            .lookup_path("src/old.rs", false)
            .unwrap()
            .elements
            .len(),
        0
    );
    let neighbors = archive.lookup_first_neighbors("active-a").unwrap();
    assert_eq!(neighbors.elements.len(), 2);
    assert_eq!(neighbors.relationships.len(), 1);
}
#[test]
fn first_neighbor_lookup_excludes_peer_and_containment_edges_from_shared_groups() {
    let db = DbCore::in_memory().unwrap();
    let elements = vec![
        element("a", "src/a.rs", "active"),
        element("b", "src/b.rs", "active"),
        element("c", "src/c.rs", "active"),
        element("parent", "src", "active"),
    ];
    let mut contains = relationship("parent", "a");
    contains.relationship_kind = "contains".into();
    contains.label = "contains".into();
    let relationships = vec![
        relationship("a", "b"),
        relationship("a", "c"),
        relationship("b", "c"),
        contains,
    ];
    db.storage_manager()
        .semantic_storage()
        .sync_semantic_structure("/repo", &elements, &relationships)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("neighbors.pz");
    db.create_semantic_snapshot("/repo", &path).unwrap();

    let lookup = PzArchive::open(&path)
        .unwrap()
        .lookup_first_neighbors("a")
        .unwrap();
    assert_eq!(
        lookup
            .relationships
            .iter()
            .map(|edge| (
                edge.source_element_id.as_str(),
                edge.target_element_id.as_str()
            ))
            .collect::<Vec<_>>(),
        vec![("a", "b"), ("a", "c")]
    );
    assert_eq!(
        lookup
            .elements
            .iter()
            .map(|node| node.semantic_element_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "c"]
    );
}

#[test]
fn cancelled_control_preserves_existing_package_and_cleans_temp() {
    let db = seeded_db();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph_db.pz");
    db.create_semantic_snapshot("/repo", &path).unwrap();
    let before = fs::read(&path).unwrap();
    let control = InvocationControl::sixty_seconds();
    control.cancel();
    assert!(
        db.create_semantic_snapshot_controlled("/repo", &path, &control)
            .is_err()
    );
    assert_eq!(before, fs::read(&path).unwrap());
    assert_eq!(
        fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .count(),
        1
    );
}

#[test]
fn preserves_only_valid_foreign_references() {
    let db = DbCore::in_memory().unwrap();
    let mut foreign_target = element("foreign-element", "src/foreign.rs", "active");
    foreign_target.project_root = "/foreign".into();
    let foreign_project = uuid::Uuid::new_v4().to_string();
    let mut foreign = relationship("active", "foreign-element");
    foreign.metadata = json!({"foreign_project_id": foreign_project});
    db.storage_manager()
        .semantic_storage()
        .sync_semantic_structure(
            "/repo",
            &[
                element("active", "src/a.rs", "active"),
                element("inactive", "src/old.rs", "inactive"),
                foreign_target,
            ],
            &[relationship("active", "inactive"), foreign],
        )
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("foreign.pz");
    let result = db.create_semantic_snapshot("/repo", &path).unwrap();
    assert_eq!(result.row_counts["external_references.parquet"], 1);
}

#[test]
fn row_group_locators_select_nonzero_groups_for_large_graphs() {
    let db = DbCore::in_memory().unwrap();
    let mut elements = Vec::with_capacity(2050);
    for index in 0..2050 {
        let id = format!("element-{index:04}");
        elements.push(element(&id, &format!("src/{id}.rs"), "active"));
    }
    let relationships = (0..2049)
        .map(|index| {
            relationship(
                &format!("element-{index:04}"),
                &format!("element-{:04}", index + 1),
            )
        })
        .collect::<Vec<_>>();
    db.storage_manager()
        .semantic_storage()
        .sync_semantic_structure("/repo", &elements, &relationships)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.pz");
    db.create_semantic_snapshot("/repo", &path).unwrap();
    let archive = PzArchive::open(&path).unwrap();
    let selected = archive.lookup_path("src/element-2049.rs", false).unwrap();
    assert_eq!(
        selected
            .elements
            .iter()
            .map(|element| element.semantic_element_id.as_str())
            .collect::<Vec<_>>(),
        vec!["element-2049"]
    );
    let neighbors = archive.lookup_first_neighbors("element-1024").unwrap();
    assert_eq!(
        neighbors
            .elements
            .iter()
            .map(|element| element.semantic_element_id.as_str())
            .collect::<Vec<_>>(),
        vec!["element-1023", "element-1024", "element-1025"],
    );
    assert_eq!(
        neighbors
            .relationships
            .iter()
            .map(|relationship| (
                relationship.source_element_id.as_str(),
                relationship.target_element_id.as_str(),
                relationship.relationship_kind.as_str(),
                relationship.label.as_str(),
            ))
            .collect::<Vec<_>>(),
        vec![
            ("element-1023", "element-1024", "calls", "calls"),
            ("element-1024", "element-1025", "calls", "calls"),
        ],
    );
}

#[test]
fn repeated_exports_keep_lineage_and_table_bytes_stable() {
    let db = seeded_db();
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.pz");
    let second = dir.path().join("second.pz");
    let one = db.create_semantic_snapshot("/repo", &first).unwrap();
    let two = db.create_semantic_snapshot("/repo", &second).unwrap();
    assert_eq!(one.project_id, two.project_id);
    assert_ne!(one.snapshot_id, two.snapshot_id);
    let a = PzArchive::open(&first).unwrap();
    let b = PzArchive::open(&second).unwrap();
    assert_eq!(a.manifest().entries, b.manifest().entries);
}

#[test]
fn failed_encode_preserves_previous_package() {
    let db = seeded_db();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph_db.pz");
    db.create_semantic_snapshot("/repo", &path).unwrap();
    let before = fs::read(&path).unwrap();
    let err = db
        .create_semantic_snapshot("/repo", dir.path().join("bad.pz"))
        .unwrap();
    assert!(err.output_bytes > 0);
    let malformed = element("bad", "/outside.rs", "active");
    db.storage_manager()
        .semantic_storage()
        .upsert_element(&malformed)
        .unwrap();
    assert!(db.create_semantic_snapshot("/repo", &path).is_err());
    assert_eq!(before, fs::read(&path).unwrap());
}
#[test]
fn exports_direct_artifacts_raw_blobs_and_typed_vectors_only_for_active_direct_owners() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .sync_semantic_structure(
            "/repo",
            &[
                element("active-a", "src/a.rs", "active"),
                element("active-b", "src/b.rs", "active"),
                element("inactive", "src/old.rs", "inactive"),
            ],
            &[],
        )
        .unwrap();
    let raw_content = vec![0xff, 0x00, 0x80, 0x41];
    let vectorizer = TestVectorizer;
    storage
        .upsert_artifact_content_with_vectorizer(
            &artifact("direct", "active-a", "direct source", json!({})),
            "application/octet-stream",
            &raw_content,
            &vectorizer,
        )
        .unwrap();
    storage
        .upsert_artifact_with_vectorizer(
            &artifact(
                "inherited",
                "active-b",
                "inherited source",
                json!({"association_kind": "inherited"}),
            ),
            &vectorizer,
        )
        .unwrap();
    storage
        .store_element_name_vectors(
            "/repo",
            &[StoredSemanticElementNameVector {
                semantic_element_id: "active-a".into(),
                project_root: "/repo".into(),
                source_text: "active-a".into(),
                vector: ArtifactTextVector {
                    engine_id: "name-engine".into(),
                    model: Some("name-model".into()),
                    dimensions: 2,
                    vector: vec![3.5, -4.25],
                    normalized: false,
                    metadata: json!({"kind": "name"}),
                },
            }],
        )
        .unwrap();
    let direct_stored = storage.artifact("direct").unwrap().unwrap();
    assert!(direct_stored.content_ref.is_some());
    assert_eq!(
        db.artifact_blobs()
            .blob(direct_stored.content_ref.as_deref().unwrap())
            .unwrap()
            .unwrap()
            .content,
        raw_content
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("typed.pz");
    let result = db.create_semantic_snapshot("/repo", &path).unwrap();
    assert_eq!(result.row_counts["artifacts.parquet"], 1);
    assert_eq!(result.row_counts["artifact_blobs.parquet"], 1);
    assert_eq!(result.row_counts["artifact_text_vectors.parquet"], 1);
    assert_eq!(result.row_counts["element_name_vectors.parquet"], 1);

    let blob_batch = first_batch(&parquet_entry(&path, "artifact_blobs.parquet"));
    assert!(matches!(
        blob_batch.schema().field(3).data_type(),
        arrow_schema::DataType::Binary
    ));
    let blob_content = blob_batch
        .column(3)
        .as_any()
        .downcast_ref::<BinaryArray>()
        .unwrap();
    assert_eq!(blob_content.value(0), raw_content.as_slice());

    let artifact_vector_batch = first_batch(&parquet_entry(&path, "artifact_text_vectors.parquet"));
    assert!(matches!(
        artifact_vector_batch.schema().field(7).data_type(),
        arrow_schema::DataType::List(_)
    ));
    let artifact_vectors = artifact_vector_batch
        .column(7)
        .as_any()
        .downcast_ref::<ListArray>()
        .unwrap();
    let artifact_vector_values = artifact_vectors.value(0);
    let artifact_vector = artifact_vector_values
        .as_any()
        .downcast_ref::<Float32Array>()
        .unwrap();
    assert_eq!(artifact_vector.values().as_ref(), &[1.25, -2.5]);
    let engine = artifact_vector_batch
        .column(3)
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    assert_eq!(engine.value(0), "test-engine");
    let metadata = artifact_vector_batch
        .column(8)
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    assert_eq!(
        metadata.value(0),
        r#"{"revision":7,"source":"focused-test"}"#
    );

    let name_vector_batch = first_batch(&parquet_entry(&path, "element_name_vectors.parquet"));
    let name_vectors = name_vector_batch
        .column(6)
        .as_any()
        .downcast_ref::<ListArray>()
        .unwrap();
    let name_vector_values = name_vectors.value(0);
    let name_vector = name_vector_values
        .as_any()
        .downcast_ref::<Float32Array>()
        .unwrap();
    assert_eq!(name_vector.values().as_ref(), &[3.5, -4.25]);
    assert_eq!(
        name_vector_batch
            .column(7)
            .as_any()
            .downcast_ref::<arrow_array::StringArray>()
            .unwrap()
            .value(0),
        r#"{"kind":"name"}"#
    );
    PzArchive::validate(&path).unwrap();
}

#[test]
fn selective_lookup_uses_validated_open_snapshot_after_source_removal() {
    let db = seeded_db();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("removed-after-open.pz");
    db.create_semantic_snapshot("/repo", &path).unwrap();
    let archive = PzArchive::open(&path).unwrap();
    fs::remove_file(&path).unwrap();

    assert_eq!(
        archive
            .lookup_path("src/a.rs", false)
            .unwrap()
            .elements
            .len(),
        2
    );
    assert_eq!(
        archive
            .lookup_first_neighbors("active-a")
            .unwrap()
            .relationships
            .len(),
        1
    );
}

#[test]
fn rejects_manifest_row_group_ranges_that_do_not_match_parquet() {
    let db = seeded_db();
    let dir = tempfile::tempdir().unwrap();

    let path = dir.path().join("invalid-row-group.pz");
    db.create_semantic_snapshot("/repo", &path).unwrap();
    rewrite_manifest(&path, |manifest| {
        let elements = manifest["entries"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|entry| entry["name"] == "elements.parquet")
            .unwrap();
        let start = elements["rowGroups"][0]["start"].as_u64().unwrap();
        elements["rowGroups"][0]["start"] = serde_json::json!(start + 1);
    });

    let error = PzArchive::validate(&path).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("manifest footer and row-group integrity metadata")
    );
}

#[test]
fn validates_zero_row_artifact_blob_and_vector_tables() {
    let db = seeded_db();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty-owned-tables.pz");
    let result = db.create_semantic_snapshot("/repo", &path).unwrap();
    for table in [
        "artifacts.parquet",
        "artifact_blobs.parquet",
        "artifact_text_vectors.parquet",
        "element_name_vectors.parquet",
    ] {
        assert_eq!(result.row_counts[table], 0);
    }
    PzArchive::validate(&path).unwrap();
}
