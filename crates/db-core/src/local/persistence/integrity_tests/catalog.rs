use super::*;

fn metadata_key(kind: &str, identity: &str) -> String {
    serde_json::to_string(&(ROOT, kind, identity)).unwrap()
}

fn recovery_key(operation_id: &str) -> String {
    metadata_key("recovery", operation_id)
}

fn record_key(record: &SemanticRecordRef) -> String {
    metadata_key("record", &serde_json::to_string(record).unwrap())
}

fn revision_row_ids(persistence: &LocalPersistence, key: &str) -> Vec<grafeo::NodeId> {
    persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .read(|graph| {
            graph
                .find_nodes_by_property("revision_row_key", &grafeo::Value::from(key))
                .into_iter()
                .filter(|id| {
                    graph
                        .get_node(*id)
                        .is_some_and(|node| node.has_label("SemanticRevisionRow"))
                })
                .collect()
        })
}

fn revision_row_payload_by_id(persistence: &LocalPersistence, id: grafeo::NodeId) -> String {
    persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .read(|graph| {
            let node = graph.get_node(id).expect("revision metadata row");
            let Some(grafeo::Value::String(payload)) = node.get_property("revision_row_json")
            else {
                panic!("expected revision metadata payload")
            };
            payload.to_string()
        })
}

fn revision_row_payload(persistence: &LocalPersistence, key: &str) -> String {
    let rows = revision_row_ids(persistence, key);
    assert_eq!(rows.len(), 1, "expected one row at {key}");
    revision_row_payload_by_id(persistence, rows[0])
}

fn revision_row_snapshot_from_graph(
    graph: &grafeo::GrafeoDB,
    id: grafeo::NodeId,
) -> (Vec<String>, Vec<(String, grafeo::Value)>) {
    let source = graph.get_node(id).expect("source revision row");
    let labels = source
        .labels
        .iter()
        .map(|label| label.as_str().to_owned())
        .collect();
    let properties = source
        .properties
        .iter()
        .map(|(name, value)| (name.as_str().to_owned(), value.clone()))
        .collect();
    (labels, properties)
}

fn revision_row_snapshot(
    persistence: &LocalPersistence,
    id: grafeo::NodeId,
) -> (Vec<String>, Vec<(String, grafeo::Value)>) {
    persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .read(|graph| revision_row_snapshot_from_graph(graph, id))
}

fn clone_revision_row(persistence: &LocalPersistence, key: &str) {
    let rows = revision_row_ids(persistence, key);
    assert_eq!(rows.len(), 1, "expected one source row at {key}");
    let (labels, properties) = revision_row_snapshot(persistence, rows[0]);
    let labels = labels.iter().map(String::as_str).collect::<Vec<_>>();
    let properties = properties
        .iter()
        .map(|(name, value)| (name.as_str(), value.clone()))
        .collect::<Vec<_>>();
    persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .write(|graph| graph.create_node_with_props(&labels, properties))
        .unwrap();
}

fn write_revision_row(
    persistence: &LocalPersistence,
    id: grafeo::NodeId,
    key: Option<String>,
    payload: String,
) {
    persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .write(|graph| {
            if let Some(key) = key {
                graph.set_node_property(id, "revision_row_key", grafeo::Value::from(key))?;
            }
            graph.set_node_property(id, "revision_row_json", grafeo::Value::from(payload))?;
            Ok(())
        })
        .unwrap();
}

fn rename_recovery_audit(persistence: &LocalPersistence, from: &str, to: &str) {
    let old_key = recovery_key(from);
    let new_key = recovery_key(to);
    let rows = revision_row_ids(persistence, &old_key);
    assert_eq!(rows.len(), 1, "expected recovery audit {from}");
    let mut payload: serde_json::Value =
        serde_json::from_str(&revision_row_payload_by_id(persistence, rows[0])).unwrap();
    payload["value"]["receipt"]["operation_id"] = serde_json::json!(to);
    write_revision_row(
        persistence,
        rows[0],
        Some(new_key),
        serde_json::to_string(&payload).unwrap(),
    );
}

fn rename_unavailable_marker(persistence: &LocalPersistence, from: &str, to: &str) {
    let key = record_key(&SemanticRecordRef::element(from));
    let rows = revision_row_ids(persistence, &key);
    assert_eq!(rows.len(), 1, "expected unavailable header {from}");
    let mut payload: serde_json::Value =
        serde_json::from_str(&revision_row_payload_by_id(persistence, rows[0])).unwrap();
    payload["value"]["unavailable"]["recovery_operation_id"] = serde_json::json!(to);
    write_revision_row(
        persistence,
        rows[0],
        None,
        serde_json::to_string(&payload).unwrap(),
    );
}

fn metadata_row_by_id(
    persistence: &LocalPersistence,
    id: grafeo::NodeId,
) -> crate::SemanticRevisionMetadata {
    serde_json::from_str(&revision_row_payload_by_id(persistence, id)).unwrap()
}

fn unique_metadata_row(
    persistence: &LocalPersistence,
    key: &str,
) -> crate::SemanticRevisionMetadata {
    let rows = revision_row_ids(persistence, key);
    assert_eq!(rows.len(), 1, "expected one metadata row at {key}");
    metadata_row_by_id(persistence, rows[0])
}

fn first_recovery_metadata_row(
    persistence: &LocalPersistence,
    operation_id: &str,
) -> crate::SemanticRevisionMetadata {
    let rows = revision_row_ids(persistence, &recovery_key(operation_id));
    assert!(!rows.is_empty(), "expected recovery audit {operation_id}");
    metadata_row_by_id(persistence, rows[0])
}

fn unavailable_source_record(persistence: &LocalPersistence) -> crate::SemanticStoredRevision {
    let crate::SemanticRevisionMetadata::Record(header) = unique_metadata_row(
        persistence,
        &record_key(&SemanticRecordRef::element("source")),
    ) else {
        panic!("expected unavailable record header")
    };
    let absence = header.payload_absence().expect("unavailable marker");
    header.restore(absence).unwrap()
}

fn valid_recovery_pair(
    persistence: &LocalPersistence,
    operation_id: &str,
) -> SemanticRevisionState {
    let crate::SemanticRevisionMetadata::Recovery(audit) =
        first_recovery_metadata_row(persistence, operation_id)
    else {
        panic!("expected recovery audit row")
    };
    SemanticRevisionState {
        project_root: ROOT.into(),
        project_id: audit.receipt.project_id.clone(),
        commit_version: audit.receipt.commit_version,
        records: vec![unavailable_source_record(persistence)],
        recovery_operations: vec![audit],
        ..Default::default()
    }
}

fn element_body_ids(persistence: &LocalPersistence, record_id: &str) -> Vec<grafeo::NodeId> {
    persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .read(|graph| {
            graph
                .find_nodes_by_property("semantic_element_id", &grafeo::Value::from(record_id))
                .into_iter()
                .filter(|id| {
                    graph
                        .get_node(*id)
                        .is_some_and(|node| node.has_label("SemanticElement"))
                })
                .collect()
        })
}

fn delete_element_body(persistence: &LocalPersistence, record_id: &str) {
    let bodies = element_body_ids(persistence, record_id);
    assert_eq!(bodies.len(), 1, "expected one body for {record_id}");
    persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .write(|graph| {
            assert!(graph.delete_node(bodies[0]));
            Ok(())
        })
        .unwrap();
}

fn recovered_source_with_intact_missing() -> (LostBodyProject, SemanticRevisionRecoveryReceipt) {
    let fixture = LostBodyProject::new();
    let receipt = fixture.recover();
    delete_element_body(&fixture.persistence, "intact");
    (fixture, receipt)
}

fn different_recovery_request(
    fixture: &LostBodyProject,
    expected_commit_version: i64,
) -> SemanticRevisionRecoveryRequest {
    SemanticRevisionRecoveryRequest {
        operation_id: "recover-intact".into(),
        expected_commit_version,
        missing_records: vec![SemanticRecordRef::element("intact")],
        ..fixture.request.clone()
    }
}

fn current_commit_version(persistence: &LocalPersistence) -> i64 {
    match execute(persistence, SemanticOperation::SemanticRevision) {
        SemanticResult::SemanticRevision { commit_version } => commit_version,
        other => panic!("{other:?}; expected semantic commit version"),
    }
}

fn assert_inspection_rejected(persistence: &LocalPersistence) {
    assert!(
        persistence
            .execute(
                SemanticOperation::InspectRevisionIntegrity {
                    publication_operation_id: None,
                    project_root: ROOT.into(),
                },
                &InvocationControl::sixty_seconds(),
            )
            .is_err()
    );
}

fn assert_new_recovery_rejected(fixture: &LostBodyProject, expected_commit_version: i64) {
    assert!(
        fixture
            .persistence
            .execute(
                SemanticOperation::RecoverRevisionIntegrity(different_recovery_request(
                    fixture,
                    expected_commit_version,
                )),
                &InvocationControl::sixty_seconds(),
            )
            .is_err()
    );
}

fn assert_catalog_corruption_blocks_new_recovery(
    fixture: &LostBodyProject,
    expected_commit_version: i64,
) {
    assert_inspection_rejected(&fixture.persistence);
    assert_new_recovery_rejected(fixture, expected_commit_version);
    assert_eq!(
        current_commit_version(&fixture.persistence),
        expected_commit_version
    );
}

#[test]
fn duplicate_physical_recovery_audit_blocks_inspection_and_new_recovery() {
    let (fixture, receipt) = recovered_source_with_intact_missing();
    let key = recovery_key("recover-source");
    let original = revision_row_payload(&fixture.persistence, &key);
    clone_revision_row(&fixture.persistence, &key);
    let rows = revision_row_ids(&fixture.persistence, &key);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        revision_row_payload_by_id(&fixture.persistence, rows[0]),
        original
    );
    assert_eq!(
        revision_row_payload_by_id(&fixture.persistence, rows[1]),
        original
    );
    valid_recovery_pair(&fixture.persistence, "recover-source")
        .validate_integrity_history()
        .unwrap();
    assert_catalog_corruption_blocks_new_recovery(&fixture, receipt.commit_version);
}

#[test]
fn publication_and_recovery_operation_id_collision_blocks_inspection_and_new_recovery() {
    let (fixture, receipt) = recovered_source_with_intact_missing();
    let publication_id = fixture.before.accepted_operations[0]
        .receipt
        .operation_id
        .clone();
    assert_eq!(publication_id, "initial-source");
    rename_recovery_audit(&fixture.persistence, "recover-source", &publication_id);
    rename_unavailable_marker(&fixture.persistence, "source", &publication_id);
    valid_recovery_pair(&fixture.persistence, &publication_id)
        .validate_integrity_history()
        .unwrap();
    assert_catalog_corruption_blocks_new_recovery(&fixture, receipt.commit_version);
}
