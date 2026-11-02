//! The private HTTP archive envelope preserves native archive fields without JavaScript number conversion.
use super::{common::*, storage_envelopes::storage};
use lumvise_semantic_revisions::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn archive_fixture() -> (ProjectFixture, Value) {
    let mut project = ProjectFixture::new();
    let mut edit = project.edit("initial", vec![element("owner", "a.rs", "old")]);
    let mut note = artifact("note", "owner", "binary");
    note.content = None;
    note.content_ref = Some("binary-ref".into());
    note.content_size_bytes = Some(4);
    note.metadata = json!({"signed":i64::MIN,"unsigned":u64::MAX});
    edit.artifacts.push(SemanticArtifactRevisionWrite {
        artifact: note,
        media_type: "application/octet-stream".into(),
        checkpoint: false,
    });
    project.publish(&edit);
    let archive = json!({
        "display_name":"Preserved project","source_revision":null,"selection_id":"working",
        "revision_state":project.state,"project_id":PROJECT_ID,"snapshot_id":"019a1d00-0000-7000-8000-000000000001","canonical_remote":null,
        "snapshot":{"project_root":ROOT,"commit_version":1,"published_at":"2026-10-26T00:00:00Z","elements":project.selected().elements,"artifacts":project.selected().artifacts,"relationships":[]},
        "artifact_blobs":[{"content_ref":"binary-ref","artifact_id":"note","media_type":"application/octet-stream","content":[0,255,128,1],"updated_at":"2026-10-26T00:00:00Z"},{"content_ref":"z-image","artifact_id":"note","media_type":"image/png","content":[255,0,128],"updated_at":"2026-10-26T00:00:00Z"},{"content_ref":"z-empty","artifact_id":"note","media_type":"application/octet-stream","content":[],"updated_at":"2026-10-26T00:00:00Z"}],
        "artifact_text_vectors":[],"element_name_vectors":[{"semantic_element_id":"owner","project_root":ROOT,"source_text":"owner","vector":{"engine_id":"engine","model":null,"dimensions":2,"vector":[1,0],"normalized":true,"metadata":{"unsigned":u64::MAX}}}],
    });
    (project, archive)
}

fn request(archive: &Value) -> String {
    json!({"operation":"import_archive","format_release":1,"operation_id":"import-1","archive_json":archive.to_string()}).to_string()
}

fn execute(state: &SemanticRevisionState, rows: &Value, request: &str) -> Value {
    json!({"action":"execute","request_json":request,"state_json":serde_json::to_string(state).unwrap(),"rows":rows,"authenticated_actor":"owner","now_ms":"9007199254740993","published_at":"2026-10-26T00:00:00Z","next_commit":(state.commit_version+1).to_string()})
}

fn import(archive: &Value) -> (SemanticRevisionState, Value, Value) {
    let initial = SemanticRevisionState {
        project_root: "/online".into(),
        project_id: PROJECT_ID.into(),
        ..Default::default()
    };
    let input = request(archive);
    let delta = storage(execute(&initial, &json!([]), &input));
    let delta = storage(
        json!({"action":"contents","delta_json":json!({"kind":"delta","value":delta}).to_string(),"request_json":input,"rows":[]}),
    );
    let rows = delta["rows"].clone();
    let state = storage(
        json!({"action":"state","project_root":"/online","project_id":PROJECT_ID,"commit_version":delta["commit_version"],"rows":rows}),
    );
    (
        serde_json::from_str(state["value_json"].as_str().unwrap()).unwrap(),
        rows,
        delta,
    )
}

pub(super) fn cases() -> Vec<Vec<u8>> {
    let (_, archive) = archive_fixture();
    let (state, rows, _) = import(&archive);
    let export = json!({"operation":"export_archive","selection_id":"working","snapshot_id":"019a1d00-0000-7000-8000-000000000002"});
    let mut cases = [
        execute(&state, &rows, &request(&archive)),
        execute(&state, &rows, &export.to_string()),
    ]
    .into_iter()
    .map(|request| serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap())
    .collect::<Vec<_>>();
    cases.extend(unavailable_archive_cases());
    cases
}

fn unavailable_archive_cases() -> Vec<Vec<u8>> {
    let (project, rows, input) = unavailable_archive_input();
    let export = execute(&project.state, &rows, &input);
    let archive = exported_archive(storage(export.clone()));
    let initial = empty_online_state();
    let import = execute(&initial, &json!([]), &request(&archive));
    let vector_request = name_vectors_request();
    let prepared = storage(json!({
        "action":"prepare",
        "request_json":vector_request.to_string(),
        "authenticated_actor":null
    }));
    let intent: Value = serde_json::from_str(prepared["value_json"].as_str().unwrap()).unwrap();
    let mut gateway = IndexedVectorGateway::new(&project.state, rows.as_array().unwrap());
    let facts = gateway.read(&intent);
    let vector_result = execute(
        &facts,
        &json!(gateway.loaded.values().cloned().collect::<Vec<_>>()),
        &vector_request.to_string(),
    );
    let mut cases = [export, import]
        .into_iter()
        .map(storage_case)
        .collect::<Vec<_>>();
    cases.push(storage_case(json!({
        "action":"prepare",
        "request_json":vector_request.to_string(),
        "authenticated_actor":null
    })));
    cases.extend(gateway.requests);
    cases.push(storage_case(vector_result));
    cases
}

fn storage_case(request: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap()
}

fn name_vectors_request() -> Value {
    json!({
        "operation":"vectors",
        "selection_id":"working",
        "artifact":false,
        "engine_id":"archive-engine",
        "model":null
    })
}

struct IndexedVectorGateway<'a> {
    state: &'a SemanticRevisionState,
    rows: &'a [Value],
    loaded: BTreeMap<String, Value>,
    completed: Vec<Value>,
    requests: Vec<Vec<u8>>,
}

impl<'a> IndexedVectorGateway<'a> {
    fn new(state: &'a SemanticRevisionState, rows: &'a [Value]) -> Self {
        Self {
            state,
            rows,
            loaded: BTreeMap::new(),
            completed: Vec::new(),
            requests: Vec::new(),
        }
    }

    fn read(&mut self, intent: &Value) -> SemanticRevisionState {
        loop {
            let request = json!({
                "action":"read",
                "context":{"project_root":ROOT,"project_id":PROJECT_ID,"commit_version":self.state.commit_version.to_string()},
                "intent_json":intent.to_string(),
                "rows":self.loaded.values().collect::<Vec<_>>(),
                "completed":self.completed
            });
            self.requests.push(storage_case(request.clone()));
            let result = storage(request);
            if let Some(state) = result["state_json"].as_str() {
                return serde_json::from_str(state).unwrap();
            }
            for query in result["queries"].as_array().unwrap() {
                assert!(
                    !self.completed.contains(query),
                    "repeated indexed vector lookup: {query}"
                );
                let matched = self
                    .rows
                    .iter()
                    .filter(|row| self.matches(row, query))
                    .cloned()
                    .collect::<Vec<_>>();
                for row in matched {
                    self.loaded.insert(row["key"].to_string(), row);
                }
                self.completed.push(query.clone());
            }
        }
    }

    fn matches(&self, row: &Value, query: &Value) -> bool {
        match query["query"].as_str().unwrap() {
            "key" => row["key"] == query["key"],
            "index" => {
                let indexed = row["indexes"].as_array().unwrap().iter().any(|index| {
                    index["name"] == query["name"] && index["value"] == query["value"]
                });
                indexed
                    && query["selection_id"]
                        .as_str()
                        .is_none_or(|selection_id| self.selected(row, selection_id))
            }
            other => panic!("unexpected vector storage query {other}"),
        }
    }

    fn selected(&self, row: &Value, selection_id: &str) -> bool {
        let Some(owner) = row_owner(row) else {
            return false;
        };
        self.state
            .selection(selection_id)
            .is_some_and(|selection| selection.records.contains(&owner))
    }
}

fn row_owner(row: &Value) -> Option<SemanticRecordRef> {
    let metadata_json = row["metadata_json"].as_str()?;
    match row["key"]["kind"].as_str()? {
        "record" => match serde_json::from_str(metadata_json).ok()? {
            SemanticRevisionMetadata::Record(header) => Some(header.record),
            _ => None,
        },
        "element_vector" | "artifact_vector" => serde_json::from_str(metadata_json).ok(),
        _ => None,
    }
}

fn unavailable_archive_input() -> (ProjectFixture, Value, String) {
    let mut project = super::citations::available_evidence_fixture();
    let vectors = write_name_vectors(&project);
    super::citations::pin_working_selection(&mut project);
    super::citations::recover_evidence(&mut project);
    super::citations::publish_rescan_successor(&mut project);
    let mut rows = storage(json!({
        "action":"rows",
        "state_json":serde_json::to_string(&project.state).unwrap()
    }));
    rows.as_array_mut()
        .unwrap()
        .extend(vectors.as_array().unwrap().iter().cloned());
    let request = json!({
        "operation":"export_archive",
        "selection_id":"working",
        "snapshot_id":"019a1d00-0000-7000-8000-000000000003"
    })
    .to_string();
    (project, rows, request)
}

fn write_name_vectors(project: &ProjectFixture) -> Value {
    let rows = storage(json!({
        "action":"rows",
        "state_json":serde_json::to_string(&project.state).unwrap()
    }));
    let request = json!({
        "operation":"store_element_name_vectors",
        "project_root":ROOT,
        "vectors":[
            {"project_root":ROOT,"semantic_element_id":"evidence","source_text":"evidence","vector":{"engine_id":"archive-engine","model":null,"dimensions":2,"vector":[0.25,0.75],"normalized":false,"metadata":{"scope":"unavailable-source"}}},
            {"project_root":ROOT,"semantic_element_id":"healthy","source_text":"healthy","vector":{"engine_id":"archive-engine","model":null,"dimensions":2,"vector":[0.5,-0.5],"normalized":false,"metadata":{"scope":"healthy-source"}}}
        ]
    })
    .to_string();
    storage(execute(&project.state, &rows, &request))["rows"].clone()
}

fn empty_online_state() -> SemanticRevisionState {
    SemanticRevisionState {
        project_root: "/online".into(),
        project_id: PROJECT_ID.into(),
        ..Default::default()
    }
}

#[test]
fn retained_unavailable_archive_preserves_history_without_substituting_successor() {
    let (project, rows, input) = unavailable_archive_input();
    let output = storage(execute(&project.state, &rows, &input));
    let exported = exported_archive(output);
    assert_eq!(exported["selection_id"], "working");
    assert_snapshot_only_has_healthy_body(&exported["snapshot"]);
    assert_healthy_vector(&exported, ROOT);
    let revision_state = &exported["revision_state"];
    assert_recovery_header_and_audit(revision_state);
    assert_working_membership(revision_state);
    assert_working_pin_history(revision_state);
    assert_successor_is_excluded(revision_state, &exported["snapshot"]);
    assert_unavailable_vector_is_rejected(&exported);
    assert_archive_import_preserves_recovery(&exported, &project);
}

#[test]
fn vector_reads_fetch_unavailable_metadata_and_never_return_its_cached_vector() {
    let (project, rows, _) = unavailable_archive_input();
    let vector_request = name_vectors_request();
    let prepared = storage(json!({
        "action":"prepare",
        "request_json":vector_request.to_string(),
        "authenticated_actor":null
    }));
    assert_eq!(prepared["kind"], "intent");
    let intent: Value = serde_json::from_str(prepared["value_json"].as_str().unwrap()).unwrap();
    assert_eq!(intent["operation"], "vectors");

    let mut gateway = IndexedVectorGateway::new(&project.state, rows.as_array().unwrap());
    let facts = gateway.read(&intent);
    assert!(gateway.completed.iter().any(|query| {
        query["query"] == "index"
            && query["name"] == "record_unavailable"
            && query["value"] == "unavailable"
            && query["selection_id"] == "working"
    }));
    assert!(gateway.completed.iter().any(|query| {
        query["query"] == "index"
            && query["name"] == "element_vector"
            && query["selection_id"] == "working"
    }));
    assert!(gateway.completed.iter().all(|query| {
        match query["query"].as_str() {
            Some("key") => ["project", "selection", "membership"]
                .contains(&query["key"]["kind"].as_str().unwrap()),
            Some("index") => {
                ["record_unavailable", "element_vector"].contains(&query["name"].as_str().unwrap())
            }
            _ => false,
        }
    }));
    assert!(
        gateway
            .loaded
            .values()
            .all(|row| { row["key"]["kind"] != "record" || row["body_json"].is_null() })
    );
    assert!(gateway.loaded.values().any(|row| {
        row["key"]["kind"] == "element_vector"
            && row_owner(row) == Some(SemanticRecordRef::element("evidence"))
    }));

    let evidence = facts
        .records
        .iter()
        .find(|record| record.record == SemanticRecordRef::element("evidence"))
        .unwrap();
    assert!(matches!(
        &evidence.body,
        SemanticRevisionBody::Unavailable(_)
    ));
    let output = storage(execute(
        &facts,
        &json!(gateway.loaded.values().cloned().collect::<Vec<_>>()),
        &vector_request.to_string(),
    ));
    let vectors: Value = serde_json::from_str(output["value_json"].as_str().unwrap()).unwrap();
    assert_eq!(vectors.as_array().unwrap().len(), 1);
    assert_eq!(vectors[0]["semantic_element_id"], "healthy");
    assert_eq!(vectors[0]["source_text"], "healthy");
    assert_eq!(vectors[0]["vector"]["engine_id"], "archive-engine");
    assert_eq!(vectors[0]["vector"]["dimensions"], 2);
    assert_eq!(vectors[0]["vector"]["vector"], json!([0.5, -0.5]));
    assert_eq!(vectors[0]["vector"]["metadata"]["scope"], "healthy-source");
    assert!(!vectors.to_string().contains("unavailable-source"));
    assert!(!vectors.to_string().contains("freshly scanned bytes"));
}

fn assert_healthy_vector(archive: &Value, project_root: &str) {
    let vectors = archive["element_name_vectors"].as_array().unwrap();
    assert_eq!(vectors.len(), 1);
    assert_eq!(vectors[0]["semantic_element_id"], "healthy");
    assert_eq!(vectors[0]["project_root"], project_root);
    assert_eq!(vectors[0]["source_text"], "healthy");
    assert_eq!(vectors[0]["vector"]["engine_id"], "archive-engine");
    assert_eq!(vectors[0]["vector"]["model"], Value::Null);
    assert_eq!(vectors[0]["vector"]["dimensions"], 2);
    assert_eq!(vectors[0]["vector"]["vector"], json!([0.5, -0.5]));
    assert_eq!(vectors[0]["vector"]["normalized"], false);
    assert_eq!(vectors[0]["vector"]["metadata"]["scope"], "healthy-source");
}

fn assert_unavailable_vector_is_rejected(archive: &Value) {
    let mut forged = archive.clone();
    forged["element_name_vectors"].as_array_mut().unwrap().push(json!({
        "project_root":ROOT,
        "semantic_element_id":"evidence",
        "source_text":"evidence",
        "vector":{"engine_id":"archive-engine","model":null,"dimensions":2,"vector":[0.25,0.75],"normalized":false,"metadata":{"scope":"unavailable-source"}}
    }));
    let response = super::storage_envelopes::response(execute(
        &empty_online_state(),
        &json!([]),
        &request(&forged),
    ));
    let message = response["message"].as_str().unwrap();
    assert_eq!(response["status"], "rejected");
    assert!(message.contains("evidence"));
    assert!(message.contains("active current element"));
    assert!(!message.contains(successor_id(archive)));
}

fn successor_id(archive: &Value) -> &str {
    archive["revision_state"]["revision_links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|link| link["older"]["record_id"] == "evidence")
        .unwrap()["newer"]["record_id"]
        .as_str()
        .unwrap()
}

fn exported_archive(output: Value) -> Value {
    let envelope: Value = serde_json::from_str(output["value_json"].as_str().unwrap()).unwrap();
    serde_json::from_str(envelope["archive_json"].as_str().unwrap()).unwrap()
}

fn assert_snapshot_only_has_healthy_body(snapshot: &Value) {
    let elements = snapshot["elements"].as_array().unwrap();
    assert_eq!(elements.len(), 1);
    assert_eq!(elements[0]["semantic_element_id"], "healthy");
    assert_eq!(elements[0]["metadata"]["body"], "healthy selected bytes");
    assert!(snapshot["artifacts"].as_array().unwrap().is_empty());
    assert!(!snapshot.to_string().contains("historical bytes"));
    assert!(!snapshot.to_string().contains("freshly scanned bytes"));
}

fn assert_recovery_header_and_audit(revision_state: &Value) {
    assert_unavailable_header(revision_state);
    assert_recovery_receipt(revision_state);
}

fn assert_unavailable_header(revision_state: &Value) {
    let unavailable = archived_record(revision_state, "evidence");
    assert_eq!(unavailable["body"]["state"], "unavailable");
    assert_eq!(
        unavailable["body"]["record"],
        json!({
            "recovery_operation_id":"acknowledge-loss",
            "unavailable_at_ms":3000,
            "canonical_lifecycle":"active"
        })
    );
    assert!(unavailable["body"]["record"].get("metadata").is_none());
}

fn assert_recovery_receipt(revision_state: &Value) {
    let accepted_recovery = &revision_state["recovery_operations"][0];
    assert_eq!(accepted_recovery["request_root"], ROOT);
    assert!(
        !accepted_recovery["request_digest"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    let audit = &accepted_recovery["receipt"];
    assert_eq!(audit["operation_id"], "acknowledge-loss");
    assert_eq!(audit["previous_commit_version"], 2);
    assert_eq!(audit["commit_version"], 3);
    assert_eq!(audit["unavailable_at_ms"], 3000);
    assert_eq!(
        audit["unavailable_records"],
        json!([{"kind":"element","record_id":"evidence"}])
    );
}

fn assert_working_membership(revision_state: &Value) {
    let working = revision_state["selections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|head| head["selection"]["selection_id"] == "working")
        .unwrap();
    assert_eq!(working["selection"]["mode"], "pinned");
    let membership = working["records"].as_array().unwrap();
    assert_eq!(membership.len(), 2);
    assert!(membership.contains(&json!({"kind":"element","record_id":"evidence"})));
    assert!(membership.contains(&json!({"kind":"element","record_id":"healthy"})));
}

fn assert_working_pin_history(revision_state: &Value) {
    let history = working_history(revision_state);
    assert_eq!(history.len(), 2);
    assert_initial_working_history(&history);
    assert_pinned_working_history(&history);
}

fn working_history(revision_state: &Value) -> Vec<&Value> {
    let history = revision_state["selection_history"].as_array().unwrap();
    history
        .iter()
        .filter(|delta| delta["selection"]["selection_id"] == "working")
        .collect()
}

fn assert_initial_working_history(history: &[&Value]) {
    let initial = history
        .iter()
        .find(|delta| delta["selection"]["generation"] == 1)
        .unwrap();
    assert!(
        initial["added_records"]
            .as_array()
            .unwrap()
            .contains(&json!({"kind":"element","record_id":"evidence"}))
    );
    assert!(
        initial["added_records"]
            .as_array()
            .unwrap()
            .contains(&json!({"kind":"element","record_id":"healthy"}))
    );
}

fn assert_pinned_working_history(history: &[&Value]) {
    let pinned = history
        .iter()
        .find(|delta| delta["selection"]["generation"] == 2)
        .unwrap();
    assert_eq!(pinned["added_records"], json!([]));
    assert_eq!(pinned["removed_records"], json!([]));
}

fn assert_successor_is_excluded(revision_state: &Value, snapshot: &Value) {
    let successor = revision_state["revision_links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|link| link["older"]["record_id"] == "evidence")
        .unwrap()["newer"]["record_id"]
        .as_str()
        .unwrap();
    let successor_body = archived_record(revision_state, successor);
    assert_eq!(successor_body["body"]["state"], "element");
    assert_eq!(
        successor_body["body"]["record"]["metadata"]["body"],
        "freshly scanned bytes"
    );
    assert!(
        snapshot["elements"]
            .as_array()
            .unwrap()
            .iter()
            .all(|element| { element["semantic_element_id"].as_str() != Some(successor) })
    );
}

fn assert_archive_import_preserves_recovery(exported: &Value, project: &ProjectFixture) {
    let (restored, rows, _) = import(exported);
    assert_imported_marker(&restored);
    assert_eq!(
        restored.recovery_operations[0],
        project.state.recovery_operations[0]
    );
    assert_eq!(
        restored.selection("working").unwrap().records,
        project.state.selection("working").unwrap().records
    );
    assert_reexported_vector(&restored, &rows);
}

fn assert_reexported_vector(state: &SemanticRevisionState, rows: &Value) {
    let request = json!({
        "operation":"export_archive",
        "selection_id":"working",
        "snapshot_id":"019a1d00-0000-7000-8000-000000000004"
    })
    .to_string();
    let output = storage(execute(state, rows, &request));
    assert_healthy_vector(&exported_archive(output), "/online");
}

fn assert_imported_marker(restored: &SemanticRevisionState) {
    let unavailable = restored
        .records
        .iter()
        .find(|record| record.record == SemanticRecordRef::element("evidence"))
        .unwrap();
    let SemanticRevisionBody::Unavailable(marker) = &unavailable.body else {
        panic!("archive import must preserve the unavailable revision marker");
    };
    assert_eq!(marker.recovery_operation_id, "acknowledge-loss");
    assert_eq!(marker.unavailable_at_ms, 3000);
}

fn archived_record<'a>(revision_state: &'a Value, id: &str) -> &'a Value {
    revision_state["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["record"]["record_id"] == id)
        .unwrap()
}

#[test]
fn archive_round_trip_keeps_binary_history_vectors_and_exact_large_values() {
    let (original, archive) = archive_fixture();
    let (state, rows, first) = import(&archive);
    let export = json!({"operation":"export_archive","selection_id":"working","snapshot_id":"019a1d00-0000-7000-8000-000000000002"});
    let output = storage(execute(&state, &rows, &export.to_string()));
    let envelope: Value = serde_json::from_str(output["value_json"].as_str().unwrap()).unwrap();
    let exported: Value = serde_json::from_str(envelope["archive_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        exported["artifact_blobs"][0]["content"],
        json!([0, 255, 128, 1])
    );
    assert_eq!(exported["artifact_blobs"].as_array().unwrap().len(), 3);
    assert_eq!(
        exported["artifact_blobs"][2]["content"],
        json!([255, 0, 128])
    );
    assert_eq!(
        exported["revision_state"]["accepted_operations"],
        serde_json::to_value(&state.accepted_operations).unwrap()
    );
    assert_eq!(
        state.accepted_operations.len(),
        original.state.accepted_operations.len()
    );
    assert_eq!(
        exported["element_name_vectors"][0]["project_root"],
        "/online"
    );
    assert_eq!(
        exported["element_name_vectors"][0]["vector"]["metadata"]["unsigned"],
        json!(u64::MAX)
    );
    let replayed = storage(execute(&state, &rows, &request(&archive)));
    assert_eq!(replayed["outcome_json"], first["outcome_json"]);
    assert_eq!(replayed["rows"], json!([]));
    assert_eq!(replayed["commit_version"], first["commit_version"]);
}

#[test]
fn changed_import_identity_and_divergent_or_missing_archive_content_are_rejected() {
    let (_, archive) = archive_fixture();
    let (state, rows, _) = import(&archive);
    for changed in ["replay", "bytes", "owner", "lineage", "working"] {
        let mut forged = archive.clone();
        match changed {
            "replay" => forged["display_name"] = json!("changed"),
            "bytes" => forged["artifact_blobs"] = json!([]),
            "owner" => forged["artifact_blobs"][0]["artifact_id"] = json!("not-an-owner"),
            "lineage" => forged["project_id"] = json!("another-project"),
            _ => forged["revision_state"]["records"][1]["body"] = json!({"kind":"pruned"}),
        }
        let mut input: Value = serde_json::from_str(&request(&forged)).unwrap();
        if changed != "replay" {
            input["operation_id"] = json!(changed);
        }
        let output: Value = serde_json::from_slice(&evaluate_revision_policy(
            &serde_json::to_vec(
                &json!({"operation":"storage","request":execute(&state,&rows,&input.to_string())}),
            )
            .unwrap(),
        ))
        .unwrap();
        assert_eq!(output["status"], "rejected", "{changed}: {output}");
    }
}

#[test]
fn archive_import_keeps_other_vector_engines_and_project_origin() {
    let (_, mut archive) = archive_fixture();
    archive["canonical_remote"] = json!("https://github.com/example/source");
    archive["source_revision"] = json!("commit-17");
    let (state, rows, _) = import(&archive);
    let mut next_archive = archive.clone();
    next_archive["element_name_vectors"][0]["vector"]["engine_id"] = json!("second");
    let mut next_request: Value = serde_json::from_str(&request(&next_archive)).unwrap();
    next_request["operation_id"] = json!("import-2");
    let delta = storage(execute(&state, &rows, &next_request.to_string()));
    let mut merged = rows.as_array().unwrap().clone();
    for row in delta["rows"].as_array().unwrap() {
        merged.retain(|old| old["key"] != row["key"]);
        merged.push(row.clone());
    }
    let export = json!({"operation":"export_archive","selection_id":"working","snapshot_id":"019a1d00-0000-7000-8000-000000000002"});
    let output = storage(execute(&state, &json!(merged), &export.to_string()));
    let envelope: Value = serde_json::from_str(output["value_json"].as_str().unwrap()).unwrap();
    let exported: Value = serde_json::from_str(envelope["archive_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        exported["element_name_vectors"].as_array().unwrap().len(),
        2
    );
    for field in ["display_name", "canonical_remote", "source_revision"] {
        assert_eq!(exported[field], archive[field], "{field}");
    }
}

#[test]
fn exhausted_archive_commit_and_nonincrementing_supplied_commit_are_rejected() {
    let (_, archive) = archive_fixture();
    let (state, rows, _) = import(&archive);
    let mut next_request: Value = serde_json::from_str(&request(&archive)).unwrap();
    next_request["operation_id"] = json!("new-import");
    let mut cases = Vec::new();
    for next in [state.commit_version, state.commit_version - 1] {
        let mut attempt = execute(&state, &rows, &next_request.to_string());
        attempt["next_commit"] = json!(next.to_string());
        cases.push(attempt);
    }
    let mut exhausted = archive.clone();
    exhausted["revision_state"]["commit_version"] = json!(i64::MAX);
    let mut exhausted_request: Value = serde_json::from_str(&request(&exhausted)).unwrap();
    exhausted_request["operation_id"] = json!("exhausted-commit");
    cases.push(execute(&state, &rows, &exhausted_request.to_string()));
    for attempt in cases {
        let result: Value = serde_json::from_slice(&evaluate_revision_policy(
            &serde_json::to_vec(&json!({"operation":"storage","request":attempt})).unwrap(),
        ))
        .unwrap();
        assert_eq!(result["status"], "rejected", "{result}");
    }
}
