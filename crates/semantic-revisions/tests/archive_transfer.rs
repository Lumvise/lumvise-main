#[path = "archive_transfer/bounded_cases.rs"]
mod bounded_cases;
mod common;
#[path = "archive_transfer/fixture.rs"]
mod fixture;
#[path = "archive_transfer/incremental.rs"]
mod incremental;

use fixture::{
    ArchiveFixture, IMPORT_IDENTITY, MERGE_NOW_MS, PROJECT_ID, RECOVERY_DIGEST, RECOVERY_ID,
    RECOVERY_TIME_MS, ROOT, SUCCESSOR_ID, archive_fixture, commit_archive, empty_state,
    existing_state,
};
use lumvise_semantic_revisions::{SemanticRevisionBody, SemanticRevisionState};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[test]
fn page_import_preserves_large_recovered_archive_and_outside_current_history() {
    let fixture = archive_fixture();
    let pages =
        lumvise_semantic_revisions::encode_archive_transfer_pages(&fixture.archive_json).unwrap();
    assert!(pages.len() > 4);
    for merge in [false, true] {
        let (before, rows) = if merge {
            existing_state(&fixture)
        } else {
            (empty_state(), json!([]))
        };
        let expected = commit_archive(&before, &rows, &fixture.archive_json, MERGE_NOW_MS);
        let mut adapter = incremental::ArchiveCatalog::new(&pages, &rows, before.commit_version);
        let accepted = adapter.run().unwrap();
        let state = fixture::storage(
            json!({"action":"state","project_root":adapter.context["project_root"],"project_id":PROJECT_ID,
            "commit_version":accepted["commit_version"],"rows":adapter.rows()}),
        );
        let state: SemanticRevisionState =
            serde_json::from_str(state["value_json"].as_str().unwrap()).unwrap();
        assert_eq!(state, expected.0);
        let applied = fixture::storage(
            json!({"action":"state","project_root":adapter.context["project_root"],"project_id":PROJECT_ID,
            "commit_version":accepted["commit_version"],"rows":adapter.applied_rows()}),
        );
        let applied: SemanticRevisionState =
            serde_json::from_str(applied["value_json"].as_str().unwrap()).unwrap();
        assert_eq!(
            applied, expected.0,
            "SQL-style rows + explicit removals must produce the complete merged candidate"
        );
        assert_recovery_history(&state, if merge { 4 } else { 3 }, if merge { 4 } else { 3 });
        assert_blob_rows(&adapter.rows(), &fixture.archive);
        assert_vector_rows(&adapter.rows());
        if merge {
            assert_outside_selection_survives(&state);
        }
        assert!(
            adapter.peak_checkpoint < 1024,
            "{}",
            adapter.peak_checkpoint
        );
    }
}

#[test]
fn newer_archive_generation_emits_real_membership_removals_for_existing_acceptance_owner() {
    let fixture = archive_fixture();
    let pages =
        lumvise_semantic_revisions::encode_archive_transfer_pages(&fixture.archive_json).unwrap();
    let (before, rows) = fixture::earlier_state(&fixture);
    assert!(
        before
            .selection("working")
            .unwrap()
            .records
            .contains(&record_ref("history-requirement"))
    );
    let expected = commit_archive(&before, &rows, &fixture.archive_json, MERGE_NOW_MS);
    let mut adapter = incremental::ArchiveCatalog::new(&pages, &rows, before.commit_version);
    let accepted = adapter.run().unwrap();
    let restored = fixture::storage(
        json!({"action":"state","project_root":before.project_root,"project_id":PROJECT_ID,
        "commit_version":accepted["commit_version"],"rows":adapter.applied_rows()}),
    );
    let restored: SemanticRevisionState =
        serde_json::from_str(restored["value_json"].as_str().unwrap()).unwrap();
    assert_eq!(restored, expected.0);
    assert!(
        !restored
            .selection("working")
            .unwrap()
            .records
            .contains(&record_ref("history-requirement"))
    );
    assert!(
        restored
            .resolve(&record_ref("history-requirement"))
            .unwrap()
            .body
            .is_available()
    );
}

#[test]
fn incremental_import_rejects_missing_history_audit_and_dependency_and_rederives_indexes() {
    let fixture = archive_fixture();
    let pages =
        lumvise_semantic_revisions::encode_archive_transfer_pages(&fixture.archive_json).unwrap();
    for missing_kind in ["history", "recovery"] {
        let mut adapter = incremental::ArchiveCatalog::new(&pages, &json!([]), 0);
        adapter
            .catalogs
            .get_mut("incoming")
            .unwrap()
            .retain(|_, row| row["key"]["kind"] != missing_kind);
        assert!(
            adapter.run().is_err(),
            "missing {missing_kind} was accepted"
        );
    }
    let mut missing = incremental::ArchiveCatalog::new(&pages, &json!([]), 0);
    missing
        .catalogs
        .get_mut("incoming")
        .unwrap()
        .retain(|_, row| {
            !row["key"]["identity"]
                .as_str()
                .unwrap()
                .contains("history-requirement")
        });
    assert!(missing.run().unwrap_err().contains("required revision"));
    let mut page: Value = serde_json::from_str(&pages[0]).unwrap();
    page["rows"][0]["indexes"] = json!([{"name":"forged","value":"authority"}]);
    let header = page["header"].to_string();
    let validated = incremental::invoke(
        json!({"action":"archive_import_page","header_json":header,"page_json":page.to_string()}),
    )
    .unwrap();
    assert!(
        !validated["rows"][0]["indexes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|index| index["name"] == "forged")
    );
    let duplicate = page["rows"][0].clone();
    page["rows"].as_array_mut().unwrap().push(duplicate);
    assert!(incremental::invoke(json!({"action":"archive_import_page","header_json":header,"page_json":page.to_string()})).unwrap_err().contains("one source row"));
    let mut adapter = incremental::ArchiveCatalog::new(&pages, &json!([]), 0);
    assert!(
        adapter
            .stage(&pages[0])
            .unwrap_err()
            .contains("duplicate source identity")
    );
    let mut invalid_record = pages
        .iter()
        .flat_map(|page| {
            let page: Value = serde_json::from_str(page).unwrap();
            page["rows"].as_array().unwrap().clone()
        })
        .find(|row| {
            row["key"]["kind"] == "record"
                && row["body_json"]
                    .as_str()
                    .is_some_and(|body| body.contains("large-record"))
        })
        .unwrap();
    let mut body: Value =
        serde_json::from_str(invalid_record["body_json"].as_str().unwrap()).unwrap();
    body["record"]["record"]["semantic_element_id"] = json!("forged-source-identity");
    invalid_record["body_json"] = json!(body.to_string());
    let invalid_page =
        json!({"format_release":2,"header":null,"rows":[invalid_record]}).to_string();
    assert!(
        incremental::invoke(
            json!({"action":"archive_import_page","header_json":header,"page_json":invalid_page})
        )
        .is_err()
    );
    let first = incremental::invoke(json!({"action":"archive_import_step","context":adapter.context,"header_json":header,"checkpoint_json":null,"facts":[]})).unwrap();
    adapter.context["expected_commit"] = json!("1");
    let stale = incremental::invoke(json!({"action":"archive_import_step","context":adapter.context,"header_json":header,"checkpoint_json":first["checkpoint_json"],"facts":[]})).unwrap_err();
    assert!(stale.contains("same frozen archive"));
}

#[test]
fn incremental_checkpoint_does_not_accumulate_a_large_record_catalog() {
    let mut project = common::ProjectFixture::new();
    project.state.project_id = PROJECT_ID.into();
    let elements = (0..512)
        .map(|i| {
            common::element(
                &format!("many-{i:04}"),
                &format!("many/{i}.rs"),
                &"B".repeat(16 * 1024),
            )
        })
        .collect();
    project.publish(&project.edit("many-records", elements));
    let archive = json!({"display_name":null,"source_revision":null,"selection_id":"working","revision_state":project.state,
        "project_id":PROJECT_ID,"snapshot_id":"many-records-v1","canonical_remote":null,
        "snapshot":{"project_root":ROOT,"commit_version":1,"published_at":"","elements":[],"artifacts":[],"relationships":[]},
        "artifact_blobs":[],"artifact_text_vectors":[],"element_name_vectors":[]});
    let pages =
        lumvise_semantic_revisions::encode_archive_transfer_pages(&archive.to_string()).unwrap();
    let mut adapter = incremental::ArchiveCatalog::new(&pages, &json!([]), 0);
    adapter.run().unwrap();
    eprintln!(
        "{}",
        json!({"case":"archive_512_x_16k","policy_calls":adapter.steps,"committed_steps":adapter.advances,
        "query_requests":adapter.total_queries,"peak_fact_bytes":adapter.peak_fact_bytes,"peak_write_bytes":adapter.peak_write_bytes,
        "peak_queries":adapter.peak_queries,"largest_row_bytes":adapter.largest_row})
    );
    assert_eq!(
        adapter.catalogs["candidate"]
            .values()
            .filter(|row| row["key"]["kind"] == "record")
            .count(),
        512
    );
    assert!(
        adapter.peak_checkpoint < 1024,
        "checkpoint grew to {} bytes",
        adapter.peak_checkpoint
    );
    // One owner row, a fixed reference batch and soft-target scan pages; never the whole archive.
    let bound = 8 * adapter.largest_row + 4 * 1024 * 1024;
    assert!(
        pages.iter().map(String::len).sum::<usize>() > bound,
        "a whole-archive fact response must exceed the asserted bound"
    );
    assert!(
        adapter.peak_fact_bytes <= bound,
        "facts {} > {bound}",
        adapter.peak_fact_bytes
    );
    assert!(
        adapter.peak_write_bytes <= bound,
        "writes {} > {bound}",
        adapter.peak_write_bytes
    );
    assert!(
        adapter.peak_queries <= 132,
        "{} queries in one step",
        adapter.peak_queries
    );
}

#[test]
fn large_recovered_archive_fresh_adoption_preserves_full_history_and_payloads() {
    let fixture = archive_fixture();
    assert_archive_fixture(&fixture);
    let imported = import_fresh(&fixture);
    assert_eq!(imported.0.project_root, "/online");
    assert_eq!(imported.0.commit_version, 8);
    assert_recovery_history(&imported.0, 3, 3);
    assert_archive_bodies(&imported.0, &imported.1, &fixture.archive);
    assert_import_receipt(&imported.1, &fixture.archive_json);
}

#[test]
fn large_recovered_archive_merge_keeps_outside_selection_and_merge_timestamps() {
    let fixture = archive_fixture();
    let (before, before_rows) = existing_state(&fixture);
    let imported = commit_archive(&before, &before_rows, &fixture.archive_json, MERGE_NOW_MS);
    assert_eq!(imported.0.project_id, PROJECT_ID);
    assert_eq!(imported.0.commit_version, 8);
    assert_outside_selection_survives(&imported.0);
    assert_eq!(
        record(&imported.0, "history-requirement").unselected_at_ms,
        Some(MERGE_NOW_MS)
    );
    assert_eq!(
        record(&imported.0, "history-requirement").pruned_at_ms,
        None
    );
    assert_recovery_history(&imported.0, 4, 4);
    assert_merge_archive_bodies(&imported.0, &imported.1, &fixture.archive);
    assert_import_receipt(&imported.1, &fixture.archive_json);
}

fn import_fresh(fixture: &ArchiveFixture) -> (SemanticRevisionState, Value, Value) {
    let empty = empty_state();
    commit_archive(&empty, &json!([]), &fixture.archive_json, MERGE_NOW_MS)
}

fn assert_archive_fixture(fixture: &ArchiveFixture) {
    assert_archive_identity(fixture);
    assert_archive_counts(fixture);
    assert_archive_payload_sizes(fixture);
}

fn assert_archive_identity(fixture: &ArchiveFixture) {
    assert!(fixture.archive_json.len() > 4 * 1024 * 1024);
    assert_eq!(fixture.archive["selection_id"], "working");
    assert_eq!(fixture.archive["project_id"], PROJECT_ID);
    assert_eq!(
        fixture.archive["snapshot_id"],
        "archive-transfer-fixture-v1"
    );
    assert_eq!(fixture.archive["snapshot"]["project_root"], ROOT);
}

fn assert_archive_counts(fixture: &ArchiveFixture) {
    assert_eq!(
        fixture.archive["snapshot"]["elements"]
            .as_array()
            .unwrap()
            .len(),
        36
    );
    assert_eq!(
        fixture.archive["snapshot"]["artifacts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_snapshot_membership(fixture);
    assert_archive_vectors_and_blobs(fixture);
}

fn assert_snapshot_membership(fixture: &ArchiveFixture) {
    let elements = fixture.archive["snapshot"]["elements"].as_array().unwrap();
    assert!(
        !elements
            .iter()
            .any(|body| body["semantic_element_id"] == "lost-record")
    );
    assert!(
        elements
            .iter()
            .any(|body| body["semantic_element_id"] == "large-record")
    );
    assert!(
        elements
            .iter()
            .any(|body| body["semantic_element_id"] == "healthy-record")
    );
}

fn assert_archive_vectors_and_blobs(fixture: &ArchiveFixture) {
    assert_eq!(
        fixture.archive["artifact_blobs"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        fixture.archive["artifact_text_vectors"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture.archive["element_name_vectors"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

fn assert_archive_payload_sizes(fixture: &ArchiveFixture) {
    let large = archived_record(&fixture.archive, "large-record");
    let payload = large["body"]["record"]["metadata"]["payload"]
        .as_str()
        .unwrap();
    assert!(payload.len() > 256 * 1024);
    assert!(
        fixture.archive["artifact_blobs"][0]["content"]
            .as_array()
            .unwrap()
            .len()
            > 256 * 1024
    );
}

fn assert_recovery_history(
    state: &SemanticRevisionState,
    history_count: usize,
    operation_count: usize,
) {
    let unavailable = record(state, "lost-record");
    let SemanticRevisionBody::Unavailable(marker) = &unavailable.body else {
        panic!("lost-record must remain unavailable with its exact recovery marker");
    };
    assert_eq!(marker.recovery_operation_id, RECOVERY_ID);
    assert_eq!(marker.unavailable_at_ms, RECOVERY_TIME_MS);
    assert_recovery_receipt(state);
    assert_publication_receipts(state, operation_count);
    assert_selection_membership(state);
    assert_selection_history(state);
    assert_eq!(state.selection_history.len(), history_count);
}

fn assert_publication_receipts(state: &SemanticRevisionState, expected_count: usize) {
    assert_eq!(state.accepted_operations.len(), expected_count);
    assert_publication_receipt(state, "archive-initial-publication", 1);
    assert_publication_receipt(state, "archive-drop-old-membership", 2);
    assert_publication_receipt(state, "source-successor", 7);
}

fn assert_publication_receipt(state: &SemanticRevisionState, id: &str, commit: i64) {
    let accepted = state
        .accepted_operations
        .iter()
        .find(|row| row.receipt.operation_id == id)
        .unwrap();
    assert_eq!(accepted.request_root, ROOT);
    assert_eq!(accepted.receipt.commit_version, commit);
}

fn assert_selection_history(state: &SemanticRevisionState) {
    assert_initial_membership_delta(state);
    assert_history_removal_delta(state);
    assert_successor_membership_delta(state);
}

fn assert_initial_membership_delta(state: &SemanticRevisionState) {
    let initial = selection_delta(state, "working", 1);
    assert_eq!(initial.added_records.len(), 39);
    assert!(
        initial
            .added_records
            .contains(&record_ref("history-requirement"))
    );
    assert!(initial.added_records.contains(&record_ref("lost-record")));
}

fn assert_history_removal_delta(state: &SemanticRevisionState) {
    let removed = selection_delta(state, "working", 2);
    assert!(removed.added_records.is_empty());
    assert_eq!(
        removed.removed_records,
        vec![record_ref("history-requirement")]
    );
}

fn assert_successor_membership_delta(state: &SemanticRevisionState) {
    let rescan = selection_delta(state, "rescan", 1);
    assert_eq!(rescan.added_records, vec![record_ref(SUCCESSOR_ID)]);
}

fn selection_delta<'a>(
    state: &'a SemanticRevisionState,
    selection_id: &str,
    generation: i64,
) -> &'a lumvise_semantic_revisions::SemanticRevisionSelectionDelta {
    state
        .selection_history
        .iter()
        .find(|delta| {
            delta.selection.selection_id == selection_id && delta.selection.generation == generation
        })
        .unwrap()
}

fn record_ref(id: &str) -> lumvise_semantic_revisions::SemanticRecordRef {
    let reference = lumvise_semantic_revisions::SemanticRecordRef::element(id);
    reference
}

fn assert_recovery_receipt(state: &SemanticRevisionState) {
    let accepted = &state.recovery_operations[0];
    assert_eq!(accepted.request_root, ROOT);
    assert_eq!(accepted.request_digest, RECOVERY_DIGEST);
    assert_eq!(accepted.receipt.operation_id, RECOVERY_ID);
    assert_eq!(accepted.receipt.project_id, PROJECT_ID);
    assert_eq!(accepted.receipt.previous_commit_version, 5);
    assert_eq!(accepted.receipt.commit_version, 6);
    assert_eq!(accepted.receipt.unavailable_at_ms, RECOVERY_TIME_MS);
    assert_eq!(
        accepted.receipt.unavailable_records,
        vec![lumvise_semantic_revisions::SemanticRecordRef::element(
            "lost-record"
        )]
    );
}

fn assert_selection_membership(state: &SemanticRevisionState) {
    let working = state.selection("working").unwrap();
    assert_eq!(working.records.len(), 38);
    assert_working_element(state, "lost-record");
    assert_working_element(state, "healthy-record");
    assert_not_working_element(state, "history-requirement");
    assert_working_artifact(state, "archive-blob");
    assert_bulk_membership(state);
}

fn assert_bulk_membership(state: &SemanticRevisionState) {
    for index in 0..34 {
        assert_working_element(state, &format!("bulk-{index:02}"));
    }
}

fn assert_working_element(state: &SemanticRevisionState, id: &str) {
    let reference = lumvise_semantic_revisions::SemanticRecordRef::element(id);
    assert!(
        state
            .selection("working")
            .unwrap()
            .records
            .contains(&reference)
    );
}

fn assert_not_working_element(state: &SemanticRevisionState, id: &str) {
    let reference = lumvise_semantic_revisions::SemanticRecordRef::element(id);
    assert!(
        !state
            .selection("working")
            .unwrap()
            .records
            .contains(&reference)
    );
}

fn assert_working_artifact(state: &SemanticRevisionState, id: &str) {
    let reference = lumvise_semantic_revisions::SemanticRecordRef::artifact(id);
    assert!(
        state
            .selection("working")
            .unwrap()
            .records
            .contains(&reference)
    );
}

fn assert_archive_bodies(state: &SemanticRevisionState, rows: &Value, archive: &Value) {
    assert_eq!(state.records.len(), 40);
    assert_eq!(record(state, SUCCESSOR_ID).record.record_id, SUCCESSOR_ID);
    assert_large_element_bodies(state);
    assert_successor_body(state);
    assert_historical_dependency(state, archive);
    assert_blob_rows(rows, archive);
    assert_vector_rows(rows);
}

fn assert_merge_archive_bodies(state: &SemanticRevisionState, rows: &Value, archive: &Value) {
    assert_eq!(state.records.len(), 41);
    assert_large_element_bodies(state);
    assert!(matches!(
        &record(state, "history-requirement").body,
        SemanticRevisionBody::Element(_)
    ));
    assert_blob_rows(rows, archive);
    assert_vector_rows(rows);
}

fn assert_large_element_bodies(state: &SemanticRevisionState) {
    assert_element_payload(record(state, "large-record"), b'L', 300 * 1024);
    for index in 0..34 {
        assert_element_payload(record(state, &format!("bulk-{index:02}")), b'B', 128 * 1024);
    }
}

fn assert_element_payload(
    stored: &lumvise_semantic_revisions::SemanticStoredRevision,
    byte: u8,
    size: usize,
) {
    assert_eq!(stored.created_at_ms, 1000);
    let SemanticRevisionBody::Element(element) = &stored.body else {
        panic!(
            "{} must retain its exact element body",
            stored.record.record_id
        );
    };
    let expected = String::from_utf8(vec![byte; size]).unwrap();
    assert_eq!(
        element.metadata["payload"].as_str().unwrap(),
        expected.as_str()
    );
}

fn assert_historical_dependency(state: &SemanticRevisionState, archive: &Value) {
    let historical = record(state, "history-requirement");
    let SemanticRevisionBody::Element(body) = &historical.body else {
        panic!("history-requirement must retain its exact dependency body");
    };
    assert_eq!(body.metadata["body"], "retained historical body");
    assert_eq!(historical.unselected_at_ms, Some(2000));
    assert_successor_dependency(archive);
}

fn assert_successor_dependency(archive: &Value) {
    let successor = archived_record(archive, SUCCESSOR_ID);
    assert_eq!(
        successor["body"]["record"]["revision_dependencies"],
        json!([{"kind":"element","record_id":"history-requirement"}])
    );
    assert!(
        archive["revision_state"]["revision_links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|link| link["older"]["record_id"] == "lost-record"
                && link["newer"]["record_id"] == SUCCESSOR_ID)
    );
}

fn assert_successor_body(state: &SemanticRevisionState) {
    let successor = record(state, SUCCESSOR_ID);
    assert_eq!(successor.created_at_ms, RECOVERY_TIME_MS + 1_000);
    let SemanticRevisionBody::Element(body) = &successor.body else {
        panic!("the distinct successor must retain its own exact body");
    };
    assert_eq!(body.metadata["body"], "distinct successor body");
}

fn assert_blob_rows(rows: &Value, archive: &Value) {
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["key"]["kind"] == "record"
                && row["key"]["identity"]
                    .as_str()
                    .unwrap()
                    .contains("archive-blob")
        })
        .unwrap();
    let body: Value = serde_json::from_str(row["body_json"].as_str().unwrap()).unwrap();
    assert_eq!(body["content"]["content_ref"], "archive-large-blob");
    assert_eq!(
        body["content"]["bytes"],
        archive["artifact_blobs"][0]["content"]
    );
    assert_eq!(body["content"]["updated_at"], "2026-10-26T00:00:00Z");
}

fn assert_vector_rows(rows: &Value) {
    assert_element_vector(rows);
    assert_artifact_vector(rows);
}

fn assert_element_vector(rows: &Value) {
    let payload = vector_payload(rows, "element_vector");
    assert_eq!(payload["kind"], "element");
    assert_eq!(
        payload["vectors"][0]["semantic_element_id"],
        "healthy-record"
    );
    assert_eq!(payload["vectors"][0]["source_text"], "healthy-record");
    assert_eq!(
        payload["vectors"][0]["vector"]["engine_id"],
        "archive-name-engine"
    );
    assert_eq!(
        payload["vectors"][0]["vector"]["vector"],
        json!([0.25, 0.75])
    );
}

fn assert_artifact_vector(rows: &Value) {
    let payload = vector_payload(rows, "artifact_vector");
    assert_eq!(payload["kind"], "artifact");
    assert_eq!(payload["vectors"][0]["artifact_id"], "archive-blob");
    assert_eq!(payload["vectors"][0]["source_text"], "archive vector text");
    assert_eq!(
        payload["vectors"][0]["vector"]["engine_id"],
        "archive-text-engine"
    );
    assert_eq!(
        payload["vectors"][0]["vector"]["vector"],
        json!([0.5, -0.5])
    );
}

fn vector_payload(rows: &Value, kind: &str) -> Value {
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"]["kind"] == kind)
        .unwrap();
    serde_json::from_str(row["body_json"].as_str().unwrap()).unwrap()
}

fn assert_outside_selection_survives(state: &SemanticRevisionState) {
    let outside = state.selection("outside").unwrap();
    assert_eq!(
        outside.records,
        vec![lumvise_semantic_revisions::SemanticRecordRef::element(
            "outside-healthy"
        )]
    );
    let outside_record = record(state, "outside-healthy");
    assert_eq!(outside_record.created_at_ms, 1000);
    let SemanticRevisionBody::Element(body) = &outside_record.body else {
        panic!("outside-healthy must retain its exact healthy body");
    };
    assert_eq!(body.metadata["body"], "outside healthy bytes");
}

fn assert_import_receipt(rows: &Value, archive_json: &str) {
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"]["kind"] == "archive_receipt")
        .unwrap();
    let receipt: Value = serde_json::from_str(row["metadata_json"].as_str().unwrap()).unwrap();
    assert_eq!(row["key"]["identity"], IMPORT_IDENTITY);
    assert_eq!(receipt["identity"], IMPORT_IDENTITY);
    assert_eq!(
        receipt["request_digest"],
        format!("{:x}", Sha256::digest(archive_json.as_bytes()))
    );
}

fn archived_record<'a>(archive: &'a Value, id: &str) -> &'a Value {
    let records = archive["revision_state"]["records"].as_array().unwrap();
    records
        .iter()
        .find(|row| row["record"]["record_id"] == id)
        .unwrap()
}

fn record<'a>(
    state: &'a SemanticRevisionState,
    id: &str,
) -> &'a lumvise_semantic_revisions::SemanticStoredRevision {
    let stored = state
        .records
        .iter()
        .find(|stored| stored.record.record_id == id)
        .unwrap();
    stored
}
