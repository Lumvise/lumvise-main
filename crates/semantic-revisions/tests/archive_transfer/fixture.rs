use crate::common::{ProjectFixture, ROOT as COMMON_ROOT, artifact, element};
use lumvise_semantic_revisions::*;
use serde_json::{Value, json};

pub(super) const ROOT: &str = COMMON_ROOT;
pub(super) const PROJECT_ID: &str = "archive-transfer-project";
pub(super) const ACTOR: &str = "archive-owner";
pub(super) const IMPORT_ID: &str = "large-recovery-import-v1";
pub(super) const IMPORT_IDENTITY: &str =
    "fbaa366b3c6bbb5a4556834f294c91936db869887f445f79f75e3416f7148941";
pub(super) const RECOVERY_ID: &str = "loss-archive-acceptance-v1";
pub(super) const RECOVERY_DIGEST: &str =
    "93e8e47bb16f1312be20f1a691c06c18a935c38a8ffc4cb87f3ee36d95eb81c9";
pub(super) const RECOVERY_TIME_MS: i64 = 7_654_321;
pub(super) const SUCCESSOR_ID: &str =
    "revision:0e0cf9253d0332c6b44c9611caf04d805e022dad5138a43a75924d08c75a1de8";
pub(super) const MERGE_NOW_MS: i64 = 9_876_543;

const BULK_ELEMENT_COUNT: usize = 34;
const BULK_PAYLOAD_BYTES: usize = 128 * 1024;
const LARGE_PAYLOAD_BYTES: usize = 300 * 1024;
const BLOB_BYTES: usize = 288 * 1024;
const VECTOR_TEXT: &str = "archive vector text";

pub(super) struct ArchiveFixture {
    pub(super) archive_json: String,
    pub(super) archive: Value,
    source_state: SemanticRevisionState,
}

pub(super) fn archive_fixture() -> ArchiveFixture {
    let (project, extra_rows) = source_project();
    project.state.validate_archive_state().unwrap();
    let rows = storage_rows(&project.state, &extra_rows);
    let exported = execute_export(&project.state, &rows);
    let archive_json = exported["value_json"].as_str().unwrap().to_owned();
    let envelope: Value = serde_json::from_str(&archive_json).unwrap();
    let archive_json = envelope["archive_json"].as_str().unwrap().to_owned();
    let archive = serde_json::from_str(&archive_json).unwrap();
    ArchiveFixture {
        archive_json,
        archive,
        source_state: project.state,
    }
}

fn source_project() -> (ProjectFixture, Vec<Value>) {
    let mut project = ProjectFixture::new();
    project.state.project_id = PROJECT_ID.into();
    publish_initial(&mut project);
    publish_without_history_dependency(&mut project);
    let mut extra_rows = Vec::new();
    write_blob(&mut project, &mut extra_rows);
    write_vectors(&mut project, &mut extra_rows);
    accept_unavailable_record(&mut project);
    publish_distinct_successor(&mut project);
    (project, extra_rows)
}

fn publish_initial(project: &mut ProjectFixture) {
    let mut publication = project.edit("archive-initial-publication", source_elements());
    let mut attachment = artifact("archive-blob", "healthy-record", VECTOR_TEXT);
    attachment.content = None;
    attachment.content_ref = Some("archive-large-blob".into());
    attachment.content_size_bytes = Some(BLOB_BYTES);
    attachment.searchable_text = Some(VECTOR_TEXT.into());
    publication.artifacts.push(SemanticArtifactRevisionWrite {
        artifact: attachment,
        media_type: "application/octet-stream".into(),
        checkpoint: false,
    });
    project.publish(&publication);
}

fn source_elements() -> Vec<SemanticElement> {
    let mut elements = vec![
        element(
            "history-requirement",
            "history.rs",
            "retained historical body",
        ),
        unavailable_candidate(),
        payload_element("large-record", "large.rs", b'L', LARGE_PAYLOAD_BYTES),
        element("healthy-record", "healthy.rs", "healthy selected body"),
    ];
    elements.extend(bulk_elements());
    elements
}

fn unavailable_candidate() -> SemanticElement {
    let mut candidate = element("lost-record", "lost.rs", "acknowledged missing body");
    candidate.metadata["provider_instance_id"] = "archive-parser".into();
    candidate
}

fn bulk_elements() -> Vec<SemanticElement> {
    (0..BULK_ELEMENT_COUNT)
        .map(|index| {
            let id = format!("bulk-{index:02}");
            let path = format!("bulk/{index:02}.rs");
            payload_element(&id, &path, b'B', BULK_PAYLOAD_BYTES)
        })
        .collect()
}

fn payload_element(id: &str, path: &str, byte: u8, size: usize) -> SemanticElement {
    let mut body = element(id, path, id);
    body.metadata = json!({"payload":String::from_utf8(vec![byte; size]).unwrap()});
    body
}

fn publish_without_history_dependency(project: &mut ProjectFixture) {
    let elements = project
        .state
        .records
        .iter()
        .filter_map(|record| match &record.body {
            SemanticRevisionBody::Element(body)
                if body.semantic_element_id != "history-requirement" =>
            {
                Some(body.clone())
            }
            _ => None,
        })
        .collect();
    let publication = project.edit("archive-drop-old-membership", elements);
    project.publish(&publication);
}

fn write_blob(project: &mut ProjectFixture, extra_rows: &mut Vec<Value>) {
    let request = json!({
        "operation":"blob_put","content_ref":"archive-large-blob","artifact_id":"archive-blob",
        "media_type":"application/octet-stream","content":blob_bytes()
    });
    commit_write(project, extra_rows, request);
}

fn blob_bytes() -> Vec<u8> {
    let bytes: Vec<u8> = (0..BLOB_BYTES).map(|index| (index % 251) as u8).collect();
    bytes
}

fn write_vectors(project: &mut ProjectFixture, extra_rows: &mut Vec<Value>) {
    commit_write(project, extra_rows, element_vector_request());
    commit_write(project, extra_rows, artifact_vector_request());
}

fn element_vector_request() -> Value {
    json!({"operation":"store_element_name_vectors","project_root":ROOT,"vectors":[{
        "semantic_element_id":"healthy-record","project_root":ROOT,"source_text":"healthy-record",
        "vector":{"engine_id":"archive-name-engine","model":"archive-v1","dimensions":2,
            "vector":[0.25,0.75],"normalized":false,"metadata":{"fixture":"large-recovered-archive"}}
    }]})
}

fn artifact_vector_request() -> Value {
    json!({"operation":"store_artifact_text_vectors","project_root":ROOT,"vectors":[{
        "artifact_id":"archive-blob","semantic_element_id":"healthy-record","source_text":VECTOR_TEXT,
        "vector":{"engine_id":"archive-text-engine","model":"archive-v1","dimensions":2,
            "vector":[0.5,-0.5],"normalized":true,"metadata":{"fixture":"large-recovered-archive"}}
    }]})
}

fn accept_unavailable_record(project: &mut ProjectFixture) {
    let report = recovery_report(project);
    let request = recovery_request(project.state.commit_version);
    let plan = report
        .plan_recovery(&request, RECOVERY_TIME_MS, project.state.commit_version + 1)
        .unwrap();
    apply_recovery(project, plan);
    project.now = RECOVERY_TIME_MS + 1_000;
}

fn recovery_report(project: &ProjectFixture) -> SemanticRevisionIntegrityReport {
    let reference = SemanticRecordRef::element("lost-record");
    let header = project
        .state
        .records
        .iter()
        .find(|row| row.record == reference)
        .map(SemanticRevisionRecordHeader::from)
        .unwrap();
    SemanticRevisionIntegrityReport {
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        commit_version: project.state.commit_version,
        missing_records: vec![header],
        publication_inspection: None,
    }
}

fn recovery_request(commit_version: i64) -> SemanticRevisionRecoveryRequest {
    SemanticRevisionRecoveryRequest {
        operation_id: RECOVERY_ID.into(),
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        expected_commit_version: commit_version,
        provider_instance_id: "archive-parser".into(),
        missing_records: vec![SemanticRecordRef::element("lost-record")],
    }
}

fn apply_recovery(project: &mut ProjectFixture, plan: SemanticRevisionRecoveryPlan) {
    let header = plan
        .headers
        .iter()
        .find(|row| row.record.record_id == "lost-record")
        .unwrap()
        .clone();
    let index = project
        .state
        .records
        .iter()
        .position(|row| row.record == header.record)
        .unwrap();
    project.state.records[index] = header
        .clone()
        .restore(header.payload_absence().unwrap())
        .unwrap();
    project.state.commit_version = plan.accepted.receipt.commit_version;
    project.state.recovery_operations.push(plan.accepted);
}

fn publish_distinct_successor(project: &mut ProjectFixture) {
    let mut successor = element(
        "lost-record",
        "lost-successor.rs",
        "distinct successor body",
    );
    successor
        .revision_dependencies
        .push(SemanticRecordRef::element("history-requirement"));
    let mut publication = project.edit("source-successor", vec![successor]);
    publication.selection_id = "rescan".into();
    publication.source_context = "git:rescan".into();
    publication.expected = SemanticSelectionExpectation::default();
    project.publish(&publication);
}

fn commit_write(project: &mut ProjectFixture, extra_rows: &mut Vec<Value>, write: Value) {
    let rows = storage_rows(&project.state, extra_rows);
    let request = write.to_string();
    let delta = execute(project, &rows, &request, project.state.commit_version + 1);
    project.state.commit_version = delta["commit_version"].as_str().unwrap().parse().unwrap();
    extend_rows(extra_rows, &delta["rows"]);
}

fn execute_export(state: &SemanticRevisionState, rows: &Value) -> Value {
    let request = json!({"operation":"export_archive","selection_id":"working","snapshot_id":"archive-transfer-fixture-v1"});
    execute_action(
        state,
        rows,
        &request.to_string(),
        state.commit_version + 1,
        ACTOR,
    )
}

fn execute(project: &ProjectFixture, rows: &Value, request_json: &str, next_commit: i64) -> Value {
    let state = &project.state;
    execute_action(state, rows, request_json, next_commit, ACTOR)
}

fn execute_action(
    state: &SemanticRevisionState,
    rows: &Value,
    request_json: &str,
    next_commit: i64,
    actor: &str,
) -> Value {
    storage(
        json!({"action":"execute","request_json":request_json,"state_json":serde_json::to_string(state).unwrap(),
        "rows":rows,"authenticated_actor":actor,"now_ms":RECOVERY_TIME_MS.to_string(),
        "published_at":"2026-10-26T00:00:00Z","next_commit":next_commit.to_string()}),
    )
}

pub(super) fn storage(request: Value) -> Value {
    let bytes = serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap();
    let response: Value = serde_json::from_slice(&evaluate_revision_policy(&bytes)).unwrap();
    assert_eq!(response["status"], "accepted", "{response}");
    response["result"]["value"]["value"].clone()
}

pub(super) fn storage_rows(state: &SemanticRevisionState, extra_rows: &[Value]) -> Value {
    let rows = storage(json!({"action":"rows","state_json":serde_json::to_string(state).unwrap()}));
    let mut combined = rows.as_array().unwrap().clone();
    combined.extend(extra_rows.iter().cloned());
    Value::Array(combined)
}

fn extend_rows(target: &mut Vec<Value>, rows: &Value) {
    let entries = rows.as_array().unwrap();
    target.extend(entries.iter().cloned());
}

pub(super) fn import_request(archive_json: &str) -> String {
    let request = json!({"operation":"import_archive","format_release":1,"operation_id":IMPORT_ID,"archive_json":archive_json});
    request.to_string()
}

pub(super) fn commit_archive(
    before: &SemanticRevisionState,
    before_rows: &Value,
    archive_json: &str,
    now_ms: i64,
) -> (SemanticRevisionState, Value, Value) {
    let request = import_request(archive_json);
    let executed = execute_import(before, before_rows, &request, now_ms);
    let delta = attach_archive_contents(executed, &request, before_rows);
    let rows = apply_delta_rows(before_rows, &delta);
    let state = restore_state(before, &delta, &rows);
    (state, rows, delta)
}

fn execute_import(
    before: &SemanticRevisionState,
    rows: &Value,
    request: &str,
    now_ms: i64,
) -> Value {
    let mut action = json!({"action":"execute","request_json":request,"state_json":serde_json::to_string(before).unwrap(),
        "rows":rows,"authenticated_actor":ACTOR,"now_ms":now_ms.to_string(),
        "published_at":"2026-10-26T00:00:00Z","next_commit":(before.commit_version + 1).to_string()});
    storage(action.take())
}

fn attach_archive_contents(executed: Value, request: &str, before_rows: &Value) -> Value {
    let encoded = json!({"kind":"delta","value":executed}).to_string();
    storage(
        json!({"action":"contents","delta_json":encoded,"request_json":request,"rows":before_rows}),
    )
}

fn apply_delta_rows(before: &Value, delta: &Value) -> Value {
    let mut rows = before.as_array().cloned().unwrap_or_default();
    remove_delta_rows(&mut rows, &delta["removed"]);
    upsert_delta_rows(&mut rows, &delta["rows"]);
    Value::Array(rows)
}

fn remove_delta_rows(rows: &mut Vec<Value>, removed: &Value) {
    for key in removed.as_array().into_iter().flatten() {
        rows.retain(|row| row.get("key") != Some(key));
    }
}

fn upsert_delta_rows(rows: &mut Vec<Value>, changed: &Value) {
    for incoming in changed.as_array().into_iter().flatten() {
        rows.retain(|row| row["key"] != incoming["key"]);
        rows.push(incoming.clone());
    }
}

fn restore_state(
    before: &SemanticRevisionState,
    delta: &Value,
    rows: &Value,
) -> SemanticRevisionState {
    let decoded = storage(
        json!({"action":"state","project_root":before.project_root,"project_id":PROJECT_ID,
        "commit_version":delta["commit_version"],"rows":rows}),
    );
    serde_json::from_str(decoded["value_json"].as_str().unwrap()).unwrap()
}

pub(super) fn empty_state() -> SemanticRevisionState {
    SemanticRevisionState {
        project_root: "/online".into(),
        project_id: PROJECT_ID.into(),
        ..Default::default()
    }
}

pub(super) fn existing_state(fixture: &ArchiveFixture) -> (SemanticRevisionState, Value) {
    let outside = outside_project();
    let mut state = fixture
        .source_state
        .merge_archive(&outside.state, MERGE_NOW_MS)
        .unwrap();
    prune_historical_dependency(&mut state);
    let rows = complete_state_rows(&state, fixture);
    (state, rows)
}

pub(super) fn earlier_state(fixture: &ArchiveFixture) -> (SemanticRevisionState, Value) {
    let mut state = fixture.source_state.clone();
    let head = state
        .selection_at_generation("working", 1)
        .unwrap()
        .unwrap();
    state
        .records
        .retain(|row| row.record.record_id != SUCCESSOR_ID);
    for row in &mut state.records {
        if head.records.contains(&row.record) {
            row.unselected_at_ms = None;
        }
    }
    state.selections = vec![head];
    state
        .selection_history
        .retain(|row| row.commit_version == 1);
    state
        .accepted_operations
        .retain(|row| row.receipt.commit_version == 1);
    state.revision_links.clear();
    state.commit_version = 6;
    state.normalize_order();
    state.validate_archive_state().unwrap();
    let rows = complete_state_rows(&state, fixture);
    (state, rows)
}

fn complete_state_rows(state: &SemanticRevisionState, fixture: &ArchiveFixture) -> Value {
    let rows = storage_rows(state, &[]);
    let delta = json!({"expected_commit":"0","commit_version":state.commit_version.to_string(),"rows":rows,"removed":[],"outcome_json":"null"});
    let complete =
        attach_archive_contents(delta, &import_request(&fixture.archive_json), &json!([]));
    complete["rows"].clone()
}

fn outside_project() -> ProjectFixture {
    let mut project = ProjectFixture::new();
    project.state.project_id = PROJECT_ID.into();
    let mut publication = project.edit(
        "outside-healthy-publication",
        vec![element(
            "outside-healthy",
            "outside.rs",
            "outside healthy bytes",
        )],
    );
    publication.selection_id = "outside".into();
    publication.source_context = "git:outside".into();
    project.publish(&publication);
    project
}

fn prune_historical_dependency(state: &mut SemanticRevisionState) {
    let stored = state
        .records
        .iter_mut()
        .find(|row| row.record == SemanticRecordRef::element("history-requirement"))
        .unwrap();
    stored.body = SemanticRevisionBody::Pruned;
    stored.pruned_at_ms = Some(6_000);
}
