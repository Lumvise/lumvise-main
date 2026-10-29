//! The private storage facade uses shared retained ancestry, including after payload pruning.
use super::{common::*, storage_envelopes::storage};
use lumvise_semantic_revisions::*;
use serde_json::{Value, json};

fn project(keep_latest: bool) -> ProjectFixture {
    let mut fixture = ProjectFixture::new();
    fixture.initial();
    let mut id = "note".to_owned();
    for revision in 1..=3 {
        let mut request = fixture.edit(&format!("note-{revision}"), vec![]);
        request.structure = None;
        request.artifacts.push(SemanticArtifactRevisionWrite {
            artifact: artifact(&id, "a", &format!("body-{revision}")),
            media_type: "text/plain".into(),
            checkpoint: revision > 1,
        });
        id = fixture.publish(&request).remaps[0]
            .accepted
            .record_id
            .clone();
    }
    if !keep_latest {
        let mut request = fixture.edit("release-last", vec![]);
        request.structure = None;
        request.removed_artifact_ids.push(id);
        fixture.publish(&request);
    }
    fixture
}

fn input(keep_latest: bool) -> Value {
    let fixture = project(keep_latest);
    let policy = SemanticRevisionRetentionPolicy::Age {
        history_age_seconds: 1,
    };
    let plan = fixture
        .state
        .plan_retention("collect", &policy, 20_000)
        .unwrap();
    let state = serde_json::to_string(&fixture.state).unwrap();
    let mut rows = storage(json!({"action":"rows","state_json":state}));
    attach_fixture_content(&mut rows);
    let blob = lumvise_contracts::ArtifactBlob {
        content_ref: "inherited-image".into(),
        artifact_id: "note".into(),
        media_type: "image/png".into(),
        content: vec![0, 255, 128, 1],
        updated_at: "2026-10-26T00:00:00Z".into(),
    };
    rows.as_array_mut().unwrap().push(json!({
        "key":{"kind":"artifact_blob","identity":blob.content_ref},
        "metadata_json":serde_json::to_string(&blob.artifact_id).unwrap(),
        "body_json":serde_json::to_string(&blob).unwrap(), "indexes":[],
    }));
    let delta = storage(json!({"action":"collection","state_json":state,
        "plan_json":serde_json::to_string(&plan).unwrap(),"policy_json":serde_json::to_string(&policy).unwrap(),
        "now_ms":"20000","next_commit":(fixture.state.commit_version+1).to_string()}));
    json!({"action":"contents","delta_json":json!({"kind":"delta","value":delta}).to_string(),
        "request_json":json!({"operation":"collect","plan":plan,"policy":policy}).to_string(),"rows":rows})
}

fn attach_fixture_content(rows: &mut Value) {
    for row in rows.as_array_mut().unwrap() {
        let Some(encoded) = row["body_json"].as_str() else {
            continue;
        };
        let mut body: Value = serde_json::from_str(encoded).unwrap();
        let record: SemanticRevisionBody = serde_json::from_value(body["record"].clone()).unwrap();
        if let SemanticRevisionBody::Artifact(artifact) = record {
            body["content"] = json!({"content_ref":artifact.content_ref,"media_type":"text/plain",
                "bytes":artifact.title.as_bytes(),"updated_at":null});
            row["body_json"] = json!(body.to_string());
        }
    }
}

pub(super) fn cases() -> Vec<Vec<u8>> {
    [true, false]
        .into_iter()
        .flat_map(|keep| [input(keep), collected_read(keep), collected_archive(keep)])
        .map(|request| {
            serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap()
        })
        .collect()
}

fn collected_read(keep: bool) -> Value {
    let request = input(keep);
    let delta = storage(request.clone());
    let mut rows = request["rows"].as_array().unwrap().clone();
    for key in delta["removed"].as_array().unwrap() {
        rows.retain(|row| row["key"] != *key);
    }
    for changed in delta["rows"].as_array().unwrap() {
        rows.retain(|row| row["key"] != changed["key"]);
        rows.push(changed.clone());
    }
    let mut fixture = project(keep);
    fixture.collect();
    json!({"action":"execute", "state_json":serde_json::to_string(&fixture.state).unwrap(), "rows":rows,
        "request_json":json!({"operation":"blob","content_ref":"inherited-image"}).to_string(),
        "authenticated_actor":"reader","now_ms":"20001","published_at":"2026-10-26T00:00:00Z",
        "next_commit":(fixture.state.commit_version+1).to_string()})
}

fn collected_archive(keep: bool) -> Value {
    let mut request = collected_read(keep);
    request["request_json"] = json!(
        json!({"operation":"export_archive","selection_id":"working",
        "snapshot_id":"019a1d00-0000-7000-8000-000000000099"})
        .to_string()
    );
    request
}

#[test]
fn archive_round_trip_keeps_extra_bytes_from_pruned_ancestors() {
    let result = storage(collected_archive(true));
    let envelope: Value = serde_json::from_str(result["value_json"].as_str().unwrap()).unwrap();
    let archive: Value = serde_json::from_str(envelope["archive_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        archive["artifact_blobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|blob| blob["content_ref"] == "inherited-image")
            .unwrap()["content"],
        json!([0, 255, 128, 1])
    );
    let state = SemanticRevisionState {
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        ..Default::default()
    };
    storage(
        json!({"action":"execute","state_json":serde_json::to_string(&state).unwrap(),"rows":[],
        "request_json":json!({"operation":"import_archive","format_release":1,"operation_id":"import-inherited","archive_json":envelope["archive_json"]}).to_string(),
        "authenticated_actor":"owner","now_ms":"20002","published_at":"2026-10-26T00:00:00Z","next_commit":"1"}),
    );
}

#[test]
fn collection_removes_attachment_bytes_only_after_all_retained_descendants_are_gone() {
    for keep in [true, false] {
        let result = storage(input(keep));
        let removed = result["removed"].as_array().unwrap();
        assert_eq!(
            removed
                .iter()
                .any(|key| key["kind"] == "artifact_blob" && key["identity"] == "inherited-image"),
            !keep
        );
        let read = storage(collected_read(keep));
        let blob: Value = serde_json::from_str(read["value_json"].as_str().unwrap()).unwrap();
        assert_eq!(blob.is_null(), !keep);
        if keep {
            assert_eq!(blob["content"], json!([0, 255, 128, 1]));
        }
    }
}
