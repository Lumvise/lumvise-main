//! The private purpose entry preserves exact public revisions and durable acceptance evidence.
use super::{common::*, storage_envelopes::storage};
use lumvise_semantic_revisions::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(super) fn cases() -> Vec<Vec<u8>> {
    let gateway = ChallengeGateway::new();
    let request = gateway.request(
        "author",
        json!({"elementId":"a","title":"review","content":"review"}),
        false,
    );
    let common = json!({"action":"challenge_execute","request_json":request["request_json"],"state_json":serde_json::to_string(&gateway.state).unwrap(),"rows":gateway.rows,"prepared_json":null,"now_ms":"3000","published_at":"2026-10-26T00:00:00Z"});
    let mut write = common.clone();
    write["prepared_json"] = json!(contribution(&request, true).to_string());
    [common,write,json!({"action":"challenge_identity","project_id":PROJECT_ID,"authenticated_actor":"author"})].into_iter().map(|request|serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap()).collect()
}

struct ChallengeGateway {
    state: SemanticRevisionState,
    rows: Vec<Value>,
}
impl ChallengeGateway {
    fn new() -> Self {
        let mut fixture = ProjectFixture::new();
        fixture.initial();
        let head = fixture.state.selection("working").unwrap();
        let update = SemanticRevisionSelectionUpdate {
            operation_id: "public-initial".into(),
            project_root: ROOT.into(),
            project_id: PROJECT_ID.into(),
            selection_id: "public".into(),
            source_context: "public:git".into(),
            expected_generation: 0,
            artifact_edits: Default::default(),
            mode: SemanticSelectionMode::Pinned,
            records: head.records.clone(),
            connection_ids: vec![],
            source_anchor: None,
        };
        let plan = fixture
            .state
            .plan_selection(&update, &timestamp(2000), 2)
            .unwrap();
        fixture.state.apply_publication(plan).unwrap();
        let rows = storage(
            json!({"action":"rows","state_json":serde_json::to_string(&fixture.state).unwrap()}),
        )
        .as_array()
        .unwrap()
        .clone();
        Self {
            state: fixture.state,
            rows,
        }
    }
    fn request(&self, actor: &str, input: Value, vote: bool) -> Value {
        let prepared = storage(
            json!({"action":"challenge_prepare","purpose":if vote {"vote"} else {"contribution"},"project_id":PROJECT_ID,"authenticated_actor":actor,"owner_override":actor == "owner","input_json":input.to_string()}),
        );
        serde_json::from_str(prepared["value_json"].as_str().unwrap()).unwrap()
    }
    fn execute(&self, request: &Value, prepared: Option<Value>) -> Value {
        storage(
            json!({"action":"challenge_execute","request_json":request["request_json"],"state_json":serde_json::to_string(&self.state).unwrap(),"rows":self.rows,"prepared_json":prepared.map(|body|body.to_string()),"now_ms":"3000","published_at":"2026-10-26T00:00:00Z"}),
        )
    }
    fn accept(&mut self, delta: &Value) {
        self.rows.retain(|row| {
            !delta["removed"].as_array().unwrap().contains(&row["key"])
                && !delta["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|next| next["key"] == row["key"])
        });
        self.rows.extend(delta["rows"].as_array().unwrap().clone());
        let state = storage(
            json!({"action":"state","project_root":ROOT,"project_id":PROJECT_ID,"commit_version":delta["commit_version"],"rows":self.rows}),
        );
        self.state = serde_json::from_str(state["value_json"].as_str().unwrap()).unwrap();
    }
}

fn contribution(request: &Value, approved: bool) -> Value {
    let trusted: Value = serde_json::from_str(request["request_json"].as_str().unwrap()).unwrap();
    let domain = request["identity_domain"].as_str().unwrap();
    let actor = trusted["actor"].as_str().unwrap();
    let mut artifact = artifact(request["report_id"].as_str().unwrap(), "a", "review");
    artifact.artifact_kind = "report".into();
    artifact.metadata = json!({"contributionId":artifact.artifact_id,"provenance":"community_contribution","contributorSubjectHash":format!("contributor:{:x}",Sha256::digest(format!("{domain}/{actor}"))),"publication":if approved {"approved"} else {"pending"},"votes":{}});
    json!({"nodes":[{"metadata":{"lumviseRecord":json!({"kind":"Artifact","value":[artifact,[]]}).to_string()}}],"status":if approved {"published"} else {"pending"}})
}

#[test]
fn purpose_publication_is_atomic_private_and_replays_exact_endpoint_outcome() {
    let mut gateway = ChallengeGateway::new();
    let request = gateway.request(
        "author",
        json!({"elementId":"a","title":"review","content":"review"}),
        false,
    );
    let delta = gateway.execute(&request, Some(contribution(&request, false)));
    gateway.accept(&delta);
    let outcome: Value = serde_json::from_str(delta["outcome_json"].as_str().unwrap()).unwrap();
    let report = outcome["reportId"].as_str().unwrap();
    assert!(
        !gateway
            .state
            .selection("public")
            .unwrap()
            .records
            .contains(&SemanticRecordRef::artifact(report))
    );
    assert!(
        gateway
            .state
            .records
            .iter()
            .any(|row| row.record == SemanticRecordRef::artifact(report))
    );
    let replay = gateway.execute(&request, None);
    let replay: Value = serde_json::from_str(replay["value_json"].as_str().unwrap()).unwrap();
    assert_eq!(replay["outcome"], outcome);
    gateway
        .rows
        .retain(|row| row["key"]["kind"] != "challenge_receipt");
    let transferred = gateway.execute(&request, None);
    let transferred: Value =
        serde_json::from_str(transferred["value_json"].as_str().unwrap()).unwrap();
    assert_eq!(transferred["unavailable"], true);
}

#[test]
fn approval_keeps_unrelated_public_members_and_exact_old_source() {
    let mut gateway = ChallengeGateway::new();
    let mut fixture = ProjectFixture {
        state: gateway.state.clone(),
        now: 3000,
    };
    let request = fixture.edit(
        "private-successor",
        vec![
            element("a", "a.rs", "private new"),
            element("b", "b.rs", "B1"),
        ],
    );
    let receipt = fixture.publish(&request);
    let successor = accepted_id(&receipt, "a");
    gateway.state = fixture.state;
    gateway.rows = storage(
        json!({"action":"rows","state_json":serde_json::to_string(&gateway.state).unwrap()}),
    )
    .as_array()
    .unwrap()
    .clone();
    let request = gateway.request(
        "author",
        json!({"elementId":"a","title":"review","content":"review"}),
        false,
    );
    let delta = gateway.execute(&request, Some(contribution(&request, true)));
    gateway.accept(&delta);
    let public = gateway.state.snapshot("public", "").unwrap().unwrap();
    assert!(
        public
            .elements
            .iter()
            .any(|element| element.semantic_element_id == "a")
    );
    assert!(
        public
            .elements
            .iter()
            .any(|element| element.semantic_element_id == "b")
    );
    assert!(
        !public
            .elements
            .iter()
            .any(|element| element.semantic_element_id == successor)
    );
    assert_eq!(public.artifacts[0].semantic_element_id, "a");
    assert!(
        gateway
            .state
            .resolve(&SemanticRecordRef::artifact(
                &public.artifacts[0].artifact_id
            ))
            .unwrap()
            .artifact_state
            .unwrap()
            .frozen
    );
}

#[test]
fn generic_requests_cannot_enter_contribution_selection_or_operation_namespace() {
    let gateway = ChallengeGateway::new();
    for reserved in [
        "knowledge-citation:someone",
        "challenge-contribution:someone",
    ] {
        let mut update = SemanticRevisionSelectionUpdate {
            operation_id: "ordinary".into(),
            project_root: ROOT.into(),
            project_id: PROJECT_ID.into(),
            selection_id: reserved.into(),
            source_context: "other".into(),
            expected_generation: 0,
            artifact_edits: Default::default(),
            mode: SemanticSelectionMode::Pinned,
            records: vec![],
            connection_ids: vec![],
            source_anchor: None,
        };
        assert!(
            gateway
                .state
                .plan_selection(&update, &timestamp(3000), 3)
                .is_err()
        );
        update.selection_id = "ordinary".into();
        update.operation_id = reserved.into();
        assert!(
            gateway
                .state
                .plan_selection(&update, &timestamp(3000), 3)
                .is_err()
        );
        let fixture = ProjectFixture {
            state: gateway.state.clone(),
            now: 3000,
        };
        let mut request = fixture.edit(reserved, vec![]);
        assert!(
            gateway
                .state
                .plan_publication(&request, &timestamp(3000), 3)
                .is_err()
        );
        request.operation_id = "ordinary".into();
        request.selection_id = reserved.into();
        assert!(
            gateway
                .state
                .plan_publication(&request, &timestamp(3000), 3)
                .is_err()
        );
        request.selection_id = "ordinary".into();
        request.source_context = reserved.into();
        assert!(
            gateway
                .state
                .plan_publication(&request, &timestamp(3000), 3)
                .is_err()
        );
        update.operation_id = "ordinary".into();
        update.source_context = reserved.into();
        assert!(
            gateway
                .state
                .plan_selection(&update, &timestamp(3000), 3)
                .is_err()
        );
        assert!(
            gateway
                .state
                .plan_retention(
                    reserved,
                    &SemanticRevisionRetentionPolicy::Age {
                        history_age_seconds: 1
                    },
                    30_000
                )
                .is_err()
        );
    }
}

#[test]
fn generic_archive_cannot_change_contribution_heads_receipts_or_working_bodies() {
    let mut gateway = ChallengeGateway::new();
    let request = gateway.request(
        "author",
        json!({"elementId":"a","title":"review","content":"review"}),
        false,
    );
    let delta = gateway.execute(&request, Some(contribution(&request, false)));
    gateway.accept(&delta);
    for mutation in ["release", "higher", "new", "receipt", "body", "context"] {
        let mut incoming = gateway.state.clone();
        let head = incoming
            .selections
            .iter_mut()
            .find(|head| {
                head.selection
                    .selection_id
                    .starts_with("challenge-contribution:")
            })
            .unwrap();
        match mutation {
            "release" => {
                head.records.clear();
                head.selection.generation += 1;
            }
            "higher" => head.selection.generation += 1,
            "new" => head.selection.selection_id.push_str("forged"),
            "context" => {
                head.selection.selection_id = "ordinary".into();
                head.selection.source_context = "challenge-contribution:forged".into();
            }
            "receipt" => incoming
                .accepted_operations
                .last_mut()
                .unwrap()
                .request_digest
                .push_str("forged"),
            _ => {
                let artifact = incoming
                    .records
                    .iter_mut()
                    .find_map(|stored| match &mut stored.body {
                        SemanticRevisionBody::Artifact(artifact) => Some(artifact),
                        _ => None,
                    })
                    .unwrap();
                artifact.title = "forged".into();
            }
        }
        assert!(
            gateway.state.merge_archive(&incoming, 4000).is_err(),
            "{mutation}"
        );
    }
    assert!(gateway.state.merge_archive(&gateway.state, 4000).is_ok());
}

#[test]
fn vote_patch_preserves_original_large_domain_numbers_and_emits_exact_edit_strings() {
    let mut gateway = ChallengeGateway::new();
    let original = gateway.request(
        "author",
        json!({"elementId":"a","title":"review","content":"review"}),
        false,
    );
    let delta = gateway.execute(&original, Some(contribution(&original, false)));
    gateway.accept(&delta);
    let id = original["report_id"].as_str().unwrap();
    let stored = gateway
        .state
        .records
        .iter_mut()
        .find(|row| row.record == SemanticRecordRef::artifact(id))
        .unwrap();
    stored.artifact_state.as_mut().unwrap().edit_version = 9_007_199_254_740_993;
    let SemanticRevisionBody::Artifact(artifact) = &mut stored.body else {
        panic!("artifact")
    };
    artifact.metadata["signed"] = json!(i64::MIN);
    artifact.metadata["unsigned"] = json!(u64::MAX);
    let mut proposed = artifact.clone();
    proposed.metadata["signed"] = json!(-9.223372036854776e18);
    proposed.metadata["unsigned"] = json!(1.8446744073709552e19);
    proposed.metadata["votes"] = json!({"voter":true});
    let old_rows = gateway.rows.clone();
    gateway.rows = storage(
        json!({"action":"rows","state_json":serde_json::to_string(&gateway.state).unwrap()}),
    )
    .as_array()
    .unwrap()
    .clone();
    for row in &mut gateway.rows {
        if row["key"]["kind"] == "record" {
            let mut body: Value = serde_json::from_str(row["body_json"].as_str().unwrap()).unwrap();
            let old = old_rows
                .iter()
                .find(|old| old["key"] == row["key"])
                .unwrap();
            let old: Value = serde_json::from_str(old["body_json"].as_str().unwrap()).unwrap();
            body["content"] = old["content"].clone();
            row["body_json"] = json!(body.to_string());
        }
    }
    let request = gateway.request("voter", json!({"reportId":id,"approve":true}), true);
    let prepared = json!({"nodes":[{"metadata":{"lumviseRecord":json!({"kind":"Artifact","value":[proposed,[]]}).to_string()}}],"receipt":{"reportId":id,"status":"pending","votes":{"yes":1,"no":0}}});
    let delta = gateway.execute(&request, Some(prepared));
    gateway.accept(&delta);
    let body = gateway
        .state
        .resolve(&SemanticRecordRef::artifact(id))
        .unwrap();
    let SemanticRevisionBody::Artifact(artifact) = body.body else {
        panic!("artifact")
    };
    assert_eq!(artifact.metadata["signed"], json!(i64::MIN));
    assert_eq!(artifact.metadata["unsigned"], json!(u64::MAX));
    let displayed = storage(
        json!({"action":"challenge_display","state_json":serde_json::to_string(&gateway.state).unwrap(),"rows":gateway.rows,"public_only":false}),
    );
    let displayed: Value = serde_json::from_str(displayed["value_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        displayed[0]["metadata"]["lumviseRevisionToken"],
        "9007199254740994"
    );
}
