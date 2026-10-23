//! Additive provider fields keep unscoped bytes and fail closed on downgraded scope.
use lumvise_db_core::{CentralizedPersistence, SemanticOperation, SemanticPartition};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
enum PriorSemanticOperation {
    SyncPartition {
        partition: PriorPartition,
        elements: Vec<()>,
        relationships: Vec<()>,
    },
    BeginProjectSnapshot {
        snapshot_id: String,
        project_root: String,
        partition_paths: Vec<String>,
        page_count: usize,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct PriorPartition {
    project_root: String,
    replace_paths: Vec<String>,
}

fn operations(provider: Option<&str>) -> [SemanticOperation; 2] {
    [
        SemanticOperation::SyncPartition {
            partition: SemanticPartition {
                project_root: "/repo".into(),
                replace_paths: vec!["src".into()],
                provider_instance_id: provider.map(str::to_owned),
            },
            elements: vec![],
            relationships: vec![],
        },
        SemanticOperation::BeginProjectSnapshot {
            snapshot_id: "snapshot".into(),
            project_root: "/repo".into(),
            partition_paths: vec!["src".into()],
            page_count: 2,
            provider_instance_id: provider.map(str::to_owned),
        },
    ]
}

fn prior_bytes(operation: &PriorSemanticOperation) -> Vec<u8> {
    let mut bytes = Vec::new();
    operation
        .serialize(
            &mut rmp_serde::Serializer::new(&mut bytes)
                .with_struct_map()
                .with_bytes(rmp_serde::config::BytesMode::ForceAll),
        )
        .unwrap();
    bytes
}

#[test]
fn omitted_provider_keeps_the_prior_binary_record_exactly() {
    for operation in operations(None) {
        let encoded = CentralizedPersistence::encode_semantic_operation(operation).unwrap();
        let prior: PriorSemanticOperation =
            rmp_serde::from_slice(&encoded.records[0].bytes).unwrap();
        assert_eq!(encoded.records[0].bytes, prior_bytes(&prior));
        assert!(matches!(
            encoded.operation_name.as_str(),
            "SyncPartition" | "BeginProjectSnapshot"
        ));
    }
}

#[test]
fn scoped_records_roundtrip_but_scope_erasure_or_operation_downgrade_is_rejected() {
    for (operation, name) in operations(Some("producer\0雪"))
        .into_iter()
        .zip(["SyncProviderPartition", "BeginProviderSnapshot"])
    {
        let encoded = CentralizedPersistence::encode_semantic_operation(operation).unwrap();
        assert_eq!(encoded.operation_name, name);
        let decoded = CentralizedPersistence::decode_semantic_operation(&encoded).unwrap();
        assert_eq!(
            serde_json::to_value(decoded)
                .unwrap()
                .to_string()
                .contains("producer\\u0000雪"),
            true
        );
        let prior: PriorSemanticOperation =
            rmp_serde::from_slice(&encoded.records[0].bytes).unwrap();
        let mut erased = encoded.clone();
        erased.records[0].bytes = prior_bytes(&prior);
        assert!(CentralizedPersistence::decode_semantic_operation(&erased).is_err());
        let mut wrong_name = encoded;
        wrong_name.operation_name = match prior {
            PriorSemanticOperation::SyncPartition { .. } => "SyncPartition",
            PriorSemanticOperation::BeginProjectSnapshot { .. } => "BeginProjectSnapshot",
        }
        .into();
        assert!(CentralizedPersistence::decode_semantic_operation(&wrong_name).is_err());
    }
}
