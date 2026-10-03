use lumvise_db_core::{
    ArtifactBlob, ArtifactDependency, ArtifactDependencyTarget, CentralizedPersistence,
    PluginDataMutation, PluginDataMutationResult, RelationalOperation, RelationalResult,
    SemanticArtifact, SemanticElement, SemanticOperation, SemanticProjectSnapshot,
    SemanticRelationship, SemanticResult, SemanticScopedGraph, SettingRecord,
};
use lumvise_resource_routing::protocol::{
    PersistenceResultV1, RelationalOperationV1, TypedBinaryChunkV1,
};
use serde_json::{Value, json};

fn element(metadata: Value) -> SemanticElement {
    SemanticElement {
        project_root: "project-root".into(),
        semantic_element_id: "element-1".into(),
        semantic_source_id: "source-1".into(),
        path: "src/main.rs".into(),
        element_kind: "function".into(),
        name: "main".into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: Some(1),
        end_line: Some(2),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata,
    }
}

fn rich_metadata() -> Value {
    json!({
        "null": null,
        "bool": true,
        "u64max": u64::MAX,
        "i64min": i64::MIN,
        "float": 1.25,
        "nul": "before\u{0}after",
        "unicode": "雪🚀"
    })
}

#[test]
fn semantic_operations_roundtrip_nested_metadata_and_tagged_dependencies() {
    let metadata = rich_metadata();
    let operation = SemanticOperation::SyncStructure {
        project_root: "project-root".into(),
        elements: vec![element(metadata.clone())],
        relationships: vec![SemanticRelationship {
            project_root: "project-root".into(),
            source_element_id: "element-1".into(),
            target_element_id: "element-2".into(),
            relationship_kind: "calls".into(),
            label: "calls".into(),
            metadata: metadata.clone(),
        }],
    };
    let encoded = CentralizedPersistence::encode_semantic_operation(operation).unwrap();
    let decoded = CentralizedPersistence::decode_semantic_operation(&encoded).unwrap();
    let SemanticOperation::SyncStructure {
        elements,
        relationships,
        ..
    } = decoded
    else {
        panic!("expected SyncStructure")
    };
    assert_eq!(elements[0].metadata, metadata);
    assert_eq!(relationships[0].metadata, metadata);

    let operation = SemanticOperation::UpsertArtifact {
        artifact: SemanticArtifact {
            artifact_id: "artifact-1".into(),
            semantic_element_id: "element-1".into(),
            artifact_kind: "specification".into(),
            title: "Unicode 雪".into(),
            content_ref: Some("content-1".into()),
            content: Some("NUL\0 and snow 雪".into()),
            searchable_text: Some("search text".into()),
            content_size_bytes: Some(17),
            dependencies: vec![
                ArtifactDependency {
                    target: ArtifactDependencyTarget::SemanticElement {
                        semantic_element_id: "element-2".into(),
                    },
                },
                ArtifactDependency {
                    target: ArtifactDependencyTarget::Artifact {
                        artifact_id: "artifact-2".into(),
                    },
                },
            ],
            metadata: metadata.clone(),
        },
        media_type: "text/plain".into(),
    };
    let encoded = CentralizedPersistence::encode_semantic_operation(operation).unwrap();
    let decoded = CentralizedPersistence::decode_semantic_operation(&encoded).unwrap();
    let SemanticOperation::UpsertArtifact { artifact, .. } = decoded else {
        panic!("expected UpsertArtifact")
    };
    assert_eq!(artifact.metadata, metadata);
    assert!(matches!(
        artifact.dependencies[0].target,
        ArtifactDependencyTarget::SemanticElement { .. }
    ));
    assert!(matches!(
        artifact.dependencies[1].target,
        ArtifactDependencyTarget::Artifact { .. }
    ));
}

#[test]
fn relational_operations_roundtrip_json_and_tagged_mutations() {
    let value = rich_metadata();
    let operation = RelationalOperation::SetPersistentSetting {
        scope: "app".into(),
        key: "nested".into(),
        value: value.clone(),
    };
    let encoded = CentralizedPersistence::encode_relational_operation(operation).unwrap();
    let decoded = CentralizedPersistence::decode_relational_operation(&encoded).unwrap();
    assert!(matches!(
        decoded,
        RelationalOperation::SetPersistentSetting { value: decoded, .. } if decoded == value
    ));

    let operation = RelationalOperation::ApplyMutations {
        plugin_id: "plugin-a".into(),
        mutations: vec![
            PluginDataMutation::Put {
                table_name: "records".into(),
                row_key: "row-1".into(),
                value: value.clone(),
            },
            PluginDataMutation::Delete {
                table_name: "records".into(),
                row_key: "row-2".into(),
            },
        ],
    };
    let encoded = CentralizedPersistence::encode_relational_operation(operation).unwrap();
    let decoded = CentralizedPersistence::decode_relational_operation(&encoded).unwrap();
    let RelationalOperation::ApplyMutations { mutations, .. } = decoded else {
        panic!("expected ApplyMutations")
    };
    assert!(matches!(
        &mutations[0],
        PluginDataMutation::Put { value: decoded, .. } if decoded == &value
    ));
    assert!(matches!(mutations[1], PluginDataMutation::Delete { .. }));
}

#[test]
fn semantic_results_roundtrip_element_snapshot_and_scoped_graph_metadata() {
    let metadata = rich_metadata();
    let one_element = element(metadata.clone());
    let snapshot = SemanticProjectSnapshot {
        commit_version: 7,
        published_at: "2026-10-03T00:00:00Z".into(),
        project_root: "project-root".into(),
        elements: vec![one_element.clone()],
        relationships: vec![],
        artifacts: vec![],
    };
    let scoped = SemanticScopedGraph {
        commit_version: 7,
        published_at: "2026-10-03T00:00:00Z".into(),
        project_root: "project-root".into(),
        elements: vec![one_element.clone()],
        relationships: vec![],
        scope_element_count: Some(1),
    };
    for result in [
        SemanticResult::Element(Some(one_element)),
        SemanticResult::ProjectSnapshot(snapshot),
        SemanticResult::ScopedGraph(scoped),
    ] {
        let encoded = CentralizedPersistence::encode_semantic_result(result).unwrap();
        let decoded = CentralizedPersistence::decode_semantic_result(&encoded).unwrap();
        let encoded_again = CentralizedPersistence::encode_semantic_result(decoded).unwrap();
        assert_eq!(encoded.operation_name, encoded_again.operation_name);
        assert_eq!(encoded.records, encoded_again.records);
    }
}

#[test]
fn relational_results_roundtrip_record_mutation_outcomes() {
    let result = RelationalResult::PersistentSetting(Some(SettingRecord {
        scope: "app".into(),
        key: "nested".into(),
        value: rich_metadata(),
        updated_at: "2026-10-03T00:00:00Z".into(),
    }));
    let encoded = CentralizedPersistence::encode_relational_result(result).unwrap();
    let decoded = CentralizedPersistence::decode_relational_result(&encoded).unwrap();
    assert!(
        matches!(decoded, RelationalResult::PersistentSetting(Some(record)) if record.value == rich_metadata())
    );

    let result = RelationalResult::PluginDataMutations(PluginDataMutationResult {
        rows_put: 1,
        rows_deleted: 1,
    });
    let encoded = CentralizedPersistence::encode_relational_result(result).unwrap();
    assert!(matches!(
        CentralizedPersistence::decode_relational_result(&encoded).unwrap(),
        RelationalResult::PluginDataMutations(PluginDataMutationResult {
            rows_put: 1,
            rows_deleted: 1
        })
    ));
}

#[test]
fn artifact_blob_records_remain_binary_sized_and_roundtrip_all_byte_values() {
    let content: Vec<u8> = (0..100_000).map(|index| index as u8).collect();
    let encoded =
        CentralizedPersistence::encode_semantic_operation(SemanticOperation::ArtifactBlobPut {
            content_ref: "blob-ref".into(),
            artifact_id: "artifact-1".into(),
            media_type: "application/octet-stream".into(),
            content: content.clone(),
        })
        .unwrap();
    assert!(encoded.records[0].bytes.len() <= content.len() + 2048);
    let decoded = CentralizedPersistence::decode_semantic_operation(&encoded).unwrap();
    assert!(matches!(
        decoded,
        SemanticOperation::ArtifactBlobPut { content: decoded, .. } if decoded == content
    ));

    let result = SemanticResult::ArtifactBlob(Some(ArtifactBlob {
        content_ref: "blob-ref".into(),
        artifact_id: "artifact-1".into(),
        media_type: "application/octet-stream".into(),
        content: content.clone(),
        updated_at: "2026-10-03T00:00:00Z".into(),
    }));
    let encoded = CentralizedPersistence::encode_semantic_result(result).unwrap();
    assert!(encoded.records[0].bytes.len() <= content.len() + 2048);
    let decoded = CentralizedPersistence::decode_semantic_result(&encoded).unwrap();
    assert!(matches!(
        decoded,
        SemanticResult::ArtifactBlob(Some(blob)) if blob.content == content
    ));
}

#[test]
fn mismatched_type_names_and_trailing_bytes_are_rejected() {
    let valid =
        CentralizedPersistence::encode_semantic_operation(SemanticOperation::SemanticRevision)
            .unwrap();
    let mut wrong_name = valid.clone();
    wrong_name.operation_name = "WrongOperation".into();
    assert!(CentralizedPersistence::decode_semantic_operation(&wrong_name).is_err());

    let mut wrong_type = valid.clone();
    wrong_type.records[0].type_name = "application/json".into();
    assert!(CentralizedPersistence::decode_semantic_operation(&wrong_type).is_err());

    let mut trailing = valid;
    trailing.records[0].bytes.push(0);
    assert!(CentralizedPersistence::decode_semantic_operation(&trailing).is_err());

    let relational = CentralizedPersistence::encode_relational_operation(
        RelationalOperation::GetPersistentSetting {
            scope: "app".into(),
            key: "theme".into(),
        },
    )
    .unwrap();
    let wrong_name = RelationalOperationV1 {
        operation_name: "WrongOperation".into(),
        ..relational.clone()
    };
    assert!(CentralizedPersistence::decode_relational_operation(&wrong_name).is_err());
    let wrong_type = RelationalOperationV1 {
        records: vec![TypedBinaryChunkV1 {
            type_name: "application/json".into(),
            bytes: relational.records[0].bytes.clone(),
            ..Default::default()
        }],
        ..relational.clone()
    };
    assert!(CentralizedPersistence::decode_relational_operation(&wrong_type).is_err());
    let mut trailing = relational;
    trailing.records[0].bytes.push(0);
    assert!(CentralizedPersistence::decode_relational_operation(&trailing).is_err());

    let encoded_result =
        CentralizedPersistence::encode_semantic_result(SemanticResult::SemanticRevision {
            commit_version: 1,
        })
        .unwrap();
    let wrong_result_type = PersistenceResultV1 {
        records: vec![TypedBinaryChunkV1 {
            type_name: "application/json".into(),
            bytes: encoded_result.records[0].bytes.clone(),
            ..Default::default()
        }],
        ..encoded_result
    };
    assert!(CentralizedPersistence::decode_semantic_result(&wrong_result_type).is_err());

    let wrong_result_name = PersistenceResultV1 {
        operation_name: "WrongResult".into(),
        ..CentralizedPersistence::encode_semantic_result(SemanticResult::SemanticRevision {
            commit_version: 1,
        })
        .unwrap()
    };
    assert!(CentralizedPersistence::decode_semantic_result(&wrong_result_name).is_err());
    let mut trailing_result =
        CentralizedPersistence::encode_semantic_result(SemanticResult::SemanticRevision {
            commit_version: 1,
        })
        .unwrap();
    trailing_result.records[0].bytes.push(0);
    assert!(CentralizedPersistence::decode_semantic_result(&trailing_result).is_err());
}
