//! The private HTTP archive envelope preserves native archive fields without JavaScript number conversion.
use super::{common::*, storage_envelopes::storage};
use lumvise_semantic_revisions::*;
use serde_json::{Value, json};

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
    [
        execute(&state, &rows, &request(&archive)),
        execute(&state, &rows, &export.to_string()),
    ]
    .into_iter()
    .map(|request| serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap())
    .collect()
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
