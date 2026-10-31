//! The private purpose entry preserves exact public revisions and durable acceptance evidence.
use super::{
    common::*,
    storage_envelopes::{response, storage},
    storage_reads::IndexedGatewayStorage,
};
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

pub(super) struct ChallengeGateway {
    pub(super) state: SemanticRevisionState,
    pub(super) rows: Vec<Value>,
}
impl ChallengeGateway {
    fn new() -> Self {
        Self::with_public("public:git", |working| working.records.clone())
    }
    /// A revision-managed project as adopted: `working` holds its elements and `public` exists but is empty.
    pub(super) fn with_empty_public_list() -> Self {
        Self::with_public("public:reports", |_| vec![])
    }
    fn with_public(
        context: &str,
        records: impl FnOnce(&SemanticRevisionSelectionState) -> Vec<SemanticRecordRef>,
    ) -> Self {
        let mut fixture = ProjectFixture::new();
        fixture.initial();
        let update = SemanticRevisionSelectionUpdate {
            operation_id: "public-initial".into(),
            project_root: ROOT.into(),
            project_id: PROJECT_ID.into(),
            selection_id: "public".into(),
            source_context: context.into(),
            expected_generation: 0,
            artifact_edits: Default::default(),
            mode: SemanticSelectionMode::Pinned,
            records: records(fixture.state.selection("working").unwrap()),
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
    pub(super) fn request(&self, actor: &str, input: Value, vote: bool) -> Value {
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
    /// Executes as the website gateway does: Rust names the rows it needs, storage serves only those,
    /// and the policy sees nothing else. An unaccepted call returns the policy's message.
    pub(super) fn scoped(&self, request: &Value, prepared: Option<Value>) -> Result<Value, String> {
        let intent = serde_json::from_str(request["intent_json"].as_str().unwrap()).unwrap();
        let mut store = IndexedGatewayStorage::with_rows(self.rows.clone());
        let facts = store.read(&self.state, intent);
        let response = response(
            json!({"action":"challenge_execute","request_json":request["request_json"],"state_json":serde_json::to_string(&facts).unwrap(),"rows":store.loaded,"prepared_json":prepared.map(|body|body.to_string()),"now_ms":(3000 + 1000 * self.state.commit_version).to_string(),"published_at":"2026-10-26T00:00:00Z"}),
        );
        if response["status"] == "accepted" {
            Ok(response["result"]["value"]["value"].clone())
        } else {
            Err(response["message"].as_str().unwrap().to_owned())
        }
    }
    pub(super) fn accept(&mut self, delta: &Value) {
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
    report(
        request,
        "a",
        "review",
        if approved { "approved" } else { "pending" },
        None,
    )
}

/// The patch the website prepares for a contributed report, pending or already decided.
pub(super) fn report(
    request: &Value,
    element: &str,
    title: &str,
    publication: &str,
    changes: Option<&str>,
) -> Value {
    let trusted: Value = serde_json::from_str(request["request_json"].as_str().unwrap()).unwrap();
    let domain = request["identity_domain"].as_str().unwrap();
    let actor = trusted["actor"].as_str().unwrap();
    let mut artifact = artifact(request["report_id"].as_str().unwrap(), element, title);
    artifact.artifact_kind = "report".into();
    artifact.metadata = json!({"contributionId":artifact.artifact_id,"provenance":"community_contribution","contributorSubjectHash":format!("contributor:{:x}",Sha256::digest(format!("{domain}/{actor}"))),"publication":publication,"votes":{}});
    if let Some(target) = changes {
        artifact.metadata["changes"] = json!(target);
    }
    json!({"nodes":[{"metadata":{"lumviseRecord":json!({"kind":"Artifact","value":[artifact,[]]}).to_string()}}],"status":if publication == "approved" {"published"} else {"pending"}})
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
fn approval_keeps_other_listed_reports_and_anchors_to_the_exact_source_revision() {
    let mut gateway = ChallengeGateway::with_empty_public_list();
    let mut fixture = ProjectFixture {
        state: gateway.state.clone(),
        now: 3000,
    };
    // A private edit moves `a` to a successor revision, which is what the current head now holds.
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
    // The creator lists two reports at once: one on `b`, one on the successor revision.
    let mut listed = vec![];
    for (source, title) in [("b", "earlier"), (successor.as_str(), "review")] {
        let request = gateway.request(
            "owner",
            json!({"elementId":source,"title":title,"content":title}),
            false,
        );
        let delta = gateway
            .scoped(
                &request,
                Some(report(&request, source, title, "approved", None)),
            )
            .unwrap();
        gateway.accept(&delta);
        listed.push(request["report_id"].as_str().unwrap().to_owned());
    }
    let public = gateway.state.snapshot("public", "").unwrap().unwrap();
    assert!(public.elements.is_empty(), "no source element is listed");
    let mut expected = listed.clone();
    expected.sort();
    let mut actual = public
        .artifacts
        .iter()
        .map(|report| report.artifact_id.clone())
        .collect::<Vec<_>>();
    actual.sort();
    assert_eq!(actual, expected, "the earlier report stays listed");
    let review = public
        .artifacts
        .iter()
        .find(|report| report.artifact_id == listed[1])
        .unwrap();
    assert_eq!(review.semantic_element_id, successor);
    assert!(
        gateway
            .state
            .resolve(&SemanticRecordRef::artifact(&review.artifact_id))
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
