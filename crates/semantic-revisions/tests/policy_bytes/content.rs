//! Exact content is part of the committed body locator; preview text is never its replacement.
use super::{common::*, storage_envelopes::storage};
use lumvise_semantic_revisions::*;
use serde_json::{Value, json};

fn text() -> String {
    "full é\0 body ".repeat(1000)
}

fn content_fixture() -> (ProjectFixture, Value, Vec<Value>) {
    let mut project = ProjectFixture::new();
    let mut edit = project.edit("initial", vec![element("owner", "a.rs", "owner")]);
    let mut note = artifact("note", "owner", "exact content");
    note.content = Some(text());
    note.metadata = json!({"signed":i64::MIN,"unsigned":u64::MAX});
    edit.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: note,
        media_type: "text/plain".into(),
    });
    let publication = json!({"action":"publication","state_json":serde_json::to_string(&project.state).unwrap(),"plan_json":serde_json::to_string(&project.plan(&edit)).unwrap()});
    let delta = storage(publication.clone());
    let contents = json!({"action":"contents","delta_json":json!({"kind":"delta","value":delta}).to_string(),"request_json":json!({"operation":"publication","request":edit}).to_string(),"rows":[]});
    let changed = storage(contents.clone())["rows"].clone();
    project.publish(&edit);
    let mut rows = storage(
        json!({"action":"rows","state_json":serde_json::to_string(&project.state).unwrap()}),
    );
    for row in rows.as_array_mut().unwrap() {
        if let Some(updated) = changed
            .as_array()
            .unwrap()
            .iter()
            .find(|updated| updated["key"] == row["key"])
        {
            *row = updated.clone();
        }
    }
    (project, rows, vec![publication, contents])
}

fn blob_request(project: &ProjectFixture, rows: &Value) -> Value {
    let reference = project.selected().artifacts[0].content_ref.clone().unwrap();
    json!({"action":"execute","request_json":json!({"operation":"blob","selection_id":"working","content_ref":reference}).to_string(),"state_json":serde_json::to_string(&project.state).unwrap(),"rows":rows,"authenticated_actor":"owner","now_ms":"9007199254740993","published_at":"time","next_commit":"9007199254741007"})
}

pub(super) fn cases() -> Vec<Vec<u8>> {
    let (project, rows, mut requests) = content_fixture();
    requests.push(blob_request(&project, &rows));
    requests
        .into_iter()
        .map(|request| {
            serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap()
        })
        .collect()
}

#[test]
fn full_content_and_exact_metadata_round_trip_through_the_stored_body_envelope() {
    let (project, rows, _) = content_fixture();
    let decoded = storage(
        json!({"action":"state","project_root":ROOT,"project_id":PROJECT_ID,"commit_version":"1","rows":rows}),
    );
    assert_eq!(
        serde_json::from_str::<SemanticRevisionState>(decoded["value_json"].as_str().unwrap())
            .unwrap(),
        project.state
    );
    assert!(
        decoded["value_json"]
            .as_str()
            .unwrap()
            .contains("18446744073709551615")
    );
    let response = storage(blob_request(&project, &rows));
    let blob: Value = serde_json::from_str(response["value_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        serde_json::from_value::<Vec<u8>>(blob["content"].clone()).unwrap(),
        text().as_bytes()
    );
    assert_eq!(blob["artifact_id"], "note");
}

#[test]
fn missing_or_mismatched_committed_bytes_fail_instead_of_returning_the_preview() {
    let (project, rows, _) = content_fixture();
    for tamper in ["missing", "length", "reference"] {
        let mut damaged = rows.clone();
        for row in damaged
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .filter(|row| row["key"]["kind"] == "record")
        {
            let mut body: Value = serde_json::from_str(row["body_json"].as_str().unwrap()).unwrap();
            if body["content"].is_null() {
                continue;
            }
            match tamper {
                "missing" => body["content"] = Value::Null,
                "length" => body["content"]["bytes"] = json!([1]),
                _ => body["content"]["content_ref"] = json!("wrong reference"),
            }
            row["body_json"] = json!(body.to_string());
        }
        let request = json!({"operation":"storage","request":blob_request(&project, &damaged)});
        let response: Value = serde_json::from_slice(&evaluate_revision_policy(
            &serde_json::to_vec(&request).unwrap(),
        ))
        .unwrap();
        assert_eq!(response["status"], "rejected", "{tamper}: {response}");
    }
}
