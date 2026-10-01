use lumvise_db_core::{
    LocalPersistence, SemanticElement, SemanticOperation, SemanticPersistence, SemanticResult,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;

#[test]
fn pz_export_preserves_claimed_identities_in_incremental_sync() {
    verify_claimed_identity_export(1, false);
    verify_claimed_identity_export(1, true);
}

#[test]
fn pz_export_preserves_claimed_identities_in_snapshot_replacement() {
    // More than 512 changed elements selects the full replacement path.
    verify_claimed_identity_export(513, false);
    verify_claimed_identity_export(513, true);
}

fn verify_claimed_identity_export(added_count: usize, existing_first: bool) {
    let persistence = LocalPersistence::in_memory().unwrap();
    sync_elements(&persistence, vec![fingerprinted_element("original")]);
    let mut incoming = (0..added_count)
        .map(|index| fingerprinted_element(&format!("added-{index}")))
        .collect::<Vec<_>>();
    let position = if existing_first { 0 } else { incoming.len() };
    incoming.insert(position, fingerprinted_element("original"));
    sync_elements(&persistence, incoming);
    let directory = tempfile::tempdir().unwrap();
    let snapshot = execute(
        &persistence,
        SemanticOperation::CreatePzSnapshot {
            project_root: "/repo".into(),
            output_path: directory
                .path()
                .join("graph.pz")
                .to_string_lossy()
                .into_owned(),
        },
    );
    let SemanticResult::PzSnapshot(snapshot) = snapshot else {
        panic!("expected PZ snapshot");
    };
    assert_eq!(
        snapshot.row_counts["elements.parquet"],
        (added_count + 1) as u64
    );
}

fn sync_elements(persistence: &LocalPersistence, elements: Vec<SemanticElement>) {
    execute(
        persistence,
        SemanticOperation::SyncStructure {
            project_root: "/repo".into(),
            elements,
            relationships: vec![],
        },
    );
}

fn execute(persistence: &LocalPersistence, operation: SemanticOperation) -> SemanticResult {
    SemanticPersistence::execute(persistence, operation, &InvocationControl::sixty_seconds())
        .expect("valid semantic operation")
}

fn fingerprinted_element(id: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".into(),
        semantic_element_id: id.into(),
        semantic_source_id: "source".into(),
        path: "src/a.rs".into(),
        element_kind: "function".into(),
        name: id.into(),
        parent_element_id: None,
        content_fingerprint: Some("fp1:0000000000000001:shared".into()),
        start_line: Some(1),
        end_line: Some(2),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}
