//! A project's `public` list starts empty and holds approved reports only; a report's source is an element of the current head.
use super::{
    common::*,
    contributions::{ChallengeGateway, report},
    storage_envelopes::storage,
    storage_reads::IndexedGatewayStorage,
};
use lumvise_semantic_revisions::*;
use serde_json::{Value, json};

fn refusal(element: &str) -> String {
    format!("invalid value {element}; expected an active element of the project's current head")
}

/// The website's two calls: the context read validates the source, then the prepared report is written.
fn contribute(
    gateway: &mut ChallengeGateway,
    actor: &str,
    element: &str,
    title: &str,
    changes: Option<&str>,
) -> Result<String, String> {
    let mut input = json!({"elementId":element,"title":title,"content":title});
    if let Some(target) = changes {
        input["changes"] = json!(target);
    }
    let request = gateway.request(actor, input, false);
    gateway.scoped(&request, None)?;
    let delta = gateway.scoped(
        &request,
        Some(report(&request, element, title, "pending", changes)),
    )?;
    gateway.accept(&delta);
    Ok(request["report_id"].as_str().unwrap().to_owned())
}

/// The tally belongs to the website; the policy applies the patch it prepares.
fn vote(
    gateway: &mut ChallengeGateway,
    voter: &str,
    report: &str,
    approve: bool,
    yes: &[&str],
    publication: &str,
) -> Result<(), String> {
    let request = gateway.request(voter, json!({"reportId":report,"approve":approve}), true);
    gateway.scoped(&request, None)?;
    let SemanticRevisionBody::Artifact(mut proposed) = gateway
        .state
        .resolve(&SemanticRecordRef::artifact(report))
        .unwrap()
        .body
    else {
        panic!("artifact")
    };
    proposed.metadata["votes"] = yes
        .iter()
        .map(|voter| ((*voter).to_owned(), json!(true)))
        .collect::<serde_json::Map<_, _>>()
        .into();
    proposed.metadata["publication"] = json!(publication);
    let patch = json!({"nodes":[{"metadata":{"lumviseRecord":json!({"kind":"Artifact","value":[proposed,[]]}).to_string()}}],"receipt":{"reportId":report,"status":publication,"votes":{"yes":yes.len(),"no":0}}});
    let delta = gateway.scoped(&request, Some(patch))?;
    gateway.accept(&delta);
    Ok(())
}

/// Two votes leave the report pending and the list untouched; the third passes it.
fn approve_by_three_votes(gateway: &mut ChallengeGateway, report: &str) {
    let before = public_reports(gateway);
    let voters = ["v1", "v2", "v3"];
    for count in 1..=3 {
        let publication = if count == 3 { "approved" } else { "pending" };
        vote(
            gateway,
            voters[count - 1],
            report,
            true,
            &voters[..count],
            publication,
        )
        .unwrap();
        if count < 3 {
            assert_eq!(public_reports(gateway), before, "vote {count} of 3");
        }
    }
}

/// Publishes an ordinary working edit, keeping the stored report bodies the gateway already holds.
fn edit_working(gateway: &mut ChallengeGateway, id: &str, elements: Vec<SemanticElement>) {
    let project = ProjectFixture {
        state: gateway.state.clone(),
        now: 3000 + 1000 * gateway.state.commit_version,
    };
    let plan = project.plan(&project.edit(id, elements));
    let delta = storage(
        json!({"action":"publication","state_json":serde_json::to_string(&gateway.state).unwrap(),"plan_json":serde_json::to_string(&plan).unwrap()}),
    );
    gateway.accept(&delta);
}

fn public_reports(gateway: &ChallengeGateway) -> Vec<SemanticRecordRef> {
    gateway.state.selection("public").unwrap().records.clone()
}

fn titles(nodes: Value) -> Vec<String> {
    let nodes: Vec<Value> = serde_json::from_str(nodes["value_json"].as_str().unwrap()).unwrap();
    let mut titles = nodes
        .iter()
        .map(|node| node["title"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    titles.sort();
    titles
}

fn displayed(gateway: &ChallengeGateway, public_only: bool) -> Vec<String> {
    titles(storage(
        json!({"action":"challenge_display","state_json":serde_json::to_string(&gateway.state).unwrap(),"rows":gateway.rows,"public_only":public_only}),
    ))
}

#[test]
fn a_contribution_on_an_empty_public_list_is_stored_pending_and_publishes_nothing() {
    let mut gateway = ChallengeGateway::with_empty_public_list();
    let request = gateway.request(
        "author",
        json!({"elementId":"a","title":"review","content":"review"}),
        false,
    );
    let context = gateway.scoped(&request, None).unwrap();
    let context: Value = serde_json::from_str(context["value_json"].as_str().unwrap()).unwrap();
    assert_eq!(context["source_element_id"], "a");
    assert_eq!(context["nodes"][0]["kind"], "semantic_element");

    let report = contribute(&mut gateway, "author", "a", "review", None).unwrap();
    assert!(public_reports(&gateway).is_empty());
    let stored = gateway
        .state
        .resolve(&SemanticRecordRef::artifact(&report))
        .unwrap();
    let SemanticRevisionBody::Artifact(stored) = stored.body else {
        panic!("artifact")
    };
    assert_eq!(stored.semantic_element_id, "a");
    assert_eq!(stored.metadata["publication"], "pending");
    let head = gateway
        .state
        .selections
        .iter()
        .find(|head| {
            head.selection
                .selection_id
                .starts_with("challenge-contribution:")
        })
        .unwrap();
    assert!(head.records.contains(&SemanticRecordRef::artifact(&report)));
}

#[test]
fn only_active_elements_of_the_current_head_can_be_reported_on() {
    let mut gateway = ChallengeGateway::with_empty_public_list();
    let mut dormant = element("dormant", "dormant.rs", "x");
    dormant.lifecycle = "inactive".into();
    // `b` leaves the head and `dormant` joins it as an inactive element.
    edit_working(
        &mut gateway,
        "later",
        vec![element("a", "a.rs", "A1"), dormant],
    );
    let head = gateway.state.selection("working").unwrap();
    assert!(
        head.records
            .contains(&SemanticRecordRef::element("dormant"))
    );
    assert!(!head.records.contains(&SemanticRecordRef::element("b")));
    for source in ["b", "dormant", "stranger"] {
        assert_eq!(
            contribute(&mut gateway, "author", source, "review", None),
            Err(refusal(source)),
            "{source}"
        );
    }
    contribute(&mut gateway, "author", "a", "review", None).unwrap();
}

#[test]
fn a_vote_needs_the_reports_source_to_stay_active_and_only_the_creators_rejection_may_skip_it() {
    let mut gateway = ChallengeGateway::with_empty_public_list();
    let report = contribute(&mut gateway, "author", "b", "review of b", None).unwrap();
    edit_working(&mut gateway, "later", vec![element("a", "a.rs", "A1")]);
    assert_eq!(
        vote(&mut gateway, "v1", &report, true, &["v1"], "pending"),
        Err(refusal("b"))
    );
    vote(&mut gateway, "owner", &report, false, &[], "rejected").unwrap();
    assert!(public_reports(&gateway).is_empty());
    assert!(
        gateway
            .state
            .selections
            .iter()
            .filter(|head| head
                .selection
                .selection_id
                .starts_with("challenge-contribution:"))
            .all(|head| head.records.is_empty()),
        "a decided report no longer pins its pending head"
    );
}

#[test]
fn approval_puts_exactly_that_report_on_the_list_and_a_public_read_shows_only_it() {
    let mut gateway = ChallengeGateway::with_empty_public_list();
    let approved = contribute(&mut gateway, "author", "a", "review of a", None).unwrap();
    contribute(&mut gateway, "someone", "b", "review of b", None).unwrap();
    approve_by_three_votes(&mut gateway, &approved);

    assert_eq!(
        public_reports(&gateway),
        vec![SemanticRecordRef::artifact(&approved)]
    );
    let public = gateway.state.snapshot("public", "").unwrap().unwrap();
    assert!(public.elements.is_empty(), "the source stays off the list");
    assert!(public.required_records.is_empty());
    assert_eq!(
        public
            .artifacts
            .iter()
            .map(|report| report.artifact_id.as_str())
            .collect::<Vec<_>>(),
        vec![approved.as_str()]
    );
    assert!(
        gateway
            .state
            .resolve(&SemanticRecordRef::artifact(&approved))
            .unwrap()
            .artifact_state
            .unwrap()
            .frozen,
        "an approved report is never revised in place"
    );
    assert_eq!(displayed(&gateway, true), vec!["review of a"]);
    assert_eq!(
        displayed(&gateway, false),
        vec!["review of a", "review of b"]
    );
}

#[test]
fn the_creators_rejection_removes_an_approved_report_even_after_its_source_left_the_head() {
    let mut gateway = ChallengeGateway::with_empty_public_list();
    let report = contribute(&mut gateway, "author", "b", "review of b", None).unwrap();
    approve_by_three_votes(&mut gateway, &report);
    edit_working(&mut gateway, "later", vec![element("a", "a.rs", "A1")]);
    vote(&mut gateway, "owner", &report, false, &[], "rejected").unwrap();
    assert!(public_reports(&gateway).is_empty());
    assert!(displayed(&gateway, true).is_empty());
}

#[test]
fn a_replacement_has_its_own_vote_and_takes_the_old_versions_place_only_when_approved() {
    let mut gateway = ChallengeGateway::with_empty_public_list();
    let old = contribute(&mut gateway, "author", "a", "first", None).unwrap();
    approve_by_three_votes(&mut gateway, &old);
    let new = contribute(&mut gateway, "author", "a", "second", Some(&old)).unwrap();
    assert_ne!(new, old);
    assert_eq!(
        public_reports(&gateway),
        vec![SemanticRecordRef::artifact(&old)],
        "the old version stays public while the change waits for its vote"
    );
    approve_by_three_votes(&mut gateway, &new);
    assert_eq!(
        public_reports(&gateway),
        vec![SemanticRecordRef::artifact(&new)]
    );
}

#[test]
fn rejecting_a_pending_replacement_leaves_the_old_version_public() {
    let mut gateway = ChallengeGateway::with_empty_public_list();
    let old = contribute(&mut gateway, "author", "a", "first", None).unwrap();
    approve_by_three_votes(&mut gateway, &old);
    let new = contribute(&mut gateway, "author", "a", "second", Some(&old)).unwrap();
    vote(&mut gateway, "owner", &new, false, &[], "rejected").unwrap();
    assert_eq!(
        public_reports(&gateway),
        vec![SemanticRecordRef::artifact(&old)]
    );
    assert_eq!(displayed(&gateway, true), vec!["first"]);
}

#[test]
fn members_enumerate_pending_reports_by_source_element_and_public_readers_see_none() {
    let mut gateway = ChallengeGateway::with_empty_public_list();
    contribute(&mut gateway, "author", "a", "review of a", None).unwrap();
    contribute(&mut gateway, "author", "b", "review of b", None).unwrap();
    let mut store = IndexedGatewayStorage::with_rows(gateway.rows.clone());
    let facts = store.read(
        &gateway.state,
        json!({"operation":"challenge_view","element_ids":["a"]}),
    );
    let view = |public_only| {
        titles(storage(
            json!({"action":"challenge_display","state_json":serde_json::to_string(&facts).unwrap(),"rows":store.loaded,"public_only":public_only}),
        ))
    };
    assert_eq!(view(false), vec!["review of a"]);
    assert!(view(true).is_empty());
}

pub(super) fn cases() -> Vec<Vec<u8>> {
    let mut gateway = ChallengeGateway::with_empty_public_list();
    let request = gateway.request(
        "author",
        json!({"elementId":"a","title":"review","content":"review"}),
        false,
    );
    let intent = serde_json::from_str(request["intent_json"].as_str().unwrap()).unwrap();
    let mut store = IndexedGatewayStorage::with_rows(gateway.rows.clone());
    let facts = store.read(&gateway.state, intent);
    let execute = |prepared: Option<Value>| json!({"action":"challenge_execute","request_json":request["request_json"],"state_json":serde_json::to_string(&facts).unwrap(),"rows":store.loaded,"prepared_json":prepared.map(|body|body.to_string()),"now_ms":"4000","published_at":"2026-10-26T00:00:00Z"});
    let mut requests = store.requests.clone();
    requests.extend(
        [
            execute(None),
            execute(Some(report(&request, "a", "review", "pending", None))),
        ]
        .into_iter()
        .map(|request| {
            serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap()
        }),
    );
    contribute(&mut gateway, "author", "a", "review", None).unwrap();
    let mut view = IndexedGatewayStorage::with_rows(gateway.rows.clone());
    view.read(
        &gateway.state,
        json!({"operation":"challenge_view","element_ids":["a"]}),
    );
    requests.extend(view.requests);
    requests
}
