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

/// The tally belongs to the website; the policy applies the patch it prepares. Returns the id the report
/// answers to afterwards: a report the owner's edit froze takes a successor revision with its first vote.
fn vote(
    gateway: &mut ChallengeGateway,
    voter: &str,
    report: &str,
    approve: bool,
    yes: &[&str],
    publication: &str,
) -> Result<String, String> {
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
    let outcome: Value = serde_json::from_str(delta["outcome_json"].as_str().unwrap()).unwrap();
    Ok(outcome["reportId"].as_str().unwrap().to_owned())
}

/// Two votes leave the report pending and the list untouched; the third passes it. Returns the final id.
fn approve_by_three_votes(gateway: &mut ChallengeGateway, report: &str) -> String {
    let before = public_reports(gateway);
    let voters = ["v1", "v2", "v3"];
    let mut report = report.to_owned();
    for count in 1..=3 {
        let publication = if count == 3 { "approved" } else { "pending" };
        report = vote(
            gateway,
            voters[count - 1],
            &report,
            true,
            &voters[..count],
            publication,
        )
        .unwrap();
        if count < 3 {
            assert_eq!(public_reports(gateway), before, "vote {count} of 3");
        }
    }
    report
}

/// Applies an owner publication the way the gateway does: the policy's delta, then the stored contents of
/// every re-emitted row, so a report the edit freezes keeps its text.
fn publish(
    gateway: &mut ChallengeGateway,
    project: &ProjectFixture,
    request: &SemanticRevisionPublication,
) -> SemanticRevisionPublicationReceipt {
    let plan = project.plan(request);
    let SemanticRevisionPublicationOutcome::Accepted(receipt) = plan.outcome.clone() else {
        panic!("accepted edit")
    };
    let delta = storage(
        json!({"action":"publication","state_json":serde_json::to_string(&gateway.state).unwrap(),"plan_json":serde_json::to_string(&plan).unwrap()}),
    );
    let delta = storage(
        json!({"action":"contents","delta_json":json!({"kind":"delta","value":delta}).to_string(),"request_json":json!({"operation":"publication","request":request}).to_string(),"rows":gateway.rows}),
    );
    gateway.accept(&delta);
    receipt
}

fn owner_edit(gateway: &ChallengeGateway) -> ProjectFixture {
    ProjectFixture {
        state: gateway.state.clone(),
        now: 3000 + 1000 * gateway.state.commit_version,
    }
}

/// Publishes an ordinary working edit, keeping the stored report bodies the gateway already holds.
fn edit_working(gateway: &mut ChallengeGateway, id: &str, elements: Vec<SemanticElement>) {
    let project = owner_edit(gateway);
    publish(gateway, &project, &project.edit(id, elements));
}

/// An owner edit that rewrites the elements under one path, as the desktop publishes it after adoption.
fn edit_path(
    gateway: &mut ChallengeGateway,
    id: &str,
    path: &str,
    elements: Vec<SemanticElement>,
) -> SemanticRevisionPublicationReceipt {
    let project = owner_edit(gateway);
    let partition = SemanticPartition {
        project_root: ROOT.into(),
        replace_paths: vec![path.into()],
        provider_instance_id: None,
    };
    let mut request = project.edit(id, elements);
    request.expected = project
        .state
        .selection_members(&SemanticRevisionSelectionQuery {
            project_root: ROOT.into(),
            selection_id: "working".into(),
            structure_coverage: SemanticRevisionStructureCoverage::Partition(partition.clone()),
            artifact_ids: vec![],
        })
        .unwrap()
        .unwrap()
        .expected;
    request.structure.as_mut().unwrap().partition = Some(partition);
    publish(gateway, &project, &request)
}

/// A paper folder with its proof next to unrelated elements, adopted under the ids `e17` and `pdf17`.
fn paper_project() -> ChallengeGateway {
    let mut gateway = ChallengeGateway::with_empty_public_list();
    let mut folder = element("e17", "paper", "paper");
    folder.element_kind = "folder".into();
    let mut proof = element("pdf17", "paper/proof.pdf", "proof");
    proof.parent_element_id = Some("e17".into());
    edit_working(
        &mut gateway,
        "adopt-paper",
        vec![
            element("a", "a.rs", "A1"),
            element("b", "b.rs", "B1"),
            folder,
            proof,
        ],
    );
    gateway
}

/// Edits the paper again, so the head swaps both records for new revisions; `retired` marks the folder
/// inactive and drops its proof.
fn revise_paper(
    gateway: &mut ChallengeGateway,
    operation: &str,
    revision: &str,
    retired: bool,
) -> SemanticRevisionPublicationReceipt {
    let mut folder = element("e17", "paper", "paper");
    folder.element_kind = "folder".into();
    folder.metadata = json!({"private": revision});
    let mut elements = vec![folder];
    if retired {
        elements[0].lifecycle = "inactive".into();
    } else {
        let mut proof = element("pdf17", "paper/proof.pdf", "proof");
        proof.parent_element_id = Some("e17".into());
        elements.push(proof);
    }
    edit_path(gateway, operation, "paper", elements)
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
fn an_element_revised_after_adoption_takes_a_contribution_and_an_approving_vote() {
    let mut gateway = paper_project();
    let receipt = revise_paper(&mut gateway, "private-newer", "v2", false);
    let (folder, proof) = (accepted_id(&receipt, "e17"), accepted_id(&receipt, "pdf17"));
    let head = gateway.state.selection("working").unwrap();
    assert!(head.records.contains(&SemanticRecordRef::element(&folder)));
    assert!(!head.records.contains(&SemanticRecordRef::element("e17")));

    // The revised elements answer to the id they were adopted under and to the revision the head holds now.
    let mut reports = vec![];
    for (source, title) in [
        ("e17", "adopted folder"),
        (folder.as_str(), "current folder"),
        ("pdf17", "adopted proof"),
        (proof.as_str(), "current proof"),
        ("a", "unrelated"),
    ] {
        reports.push(contribute(&mut gateway, "author", source, title, None).unwrap());
    }
    approve_by_three_votes(&mut gateway, &reports[0]);

    assert_eq!(
        public_reports(&gateway),
        vec![SemanticRecordRef::artifact(&reports[0])]
    );
    let SemanticRevisionBody::Artifact(approved) = gateway
        .state
        .resolve(&SemanticRecordRef::artifact(&reports[0]))
        .unwrap()
        .body
    else {
        panic!("artifact")
    };
    assert_eq!(
        approved.semantic_element_id, "e17",
        "the report keeps the exact revision it was written against"
    );
    let public = gateway.state.snapshot("public", "").unwrap().unwrap();
    assert!(public.elements.is_empty());
    // The exact source revision stays stored for as long as the approved report is public, while an
    // unreferenced superseded revision is collectable.
    revise_paper(&mut gateway, "edit-again", "v3", false);
    edit_path(
        &mut gateway,
        "edit-b",
        "b.rs",
        vec![element("b", "b.rs", "B2")],
    );
    let policy = SemanticRevisionRetentionPolicy::Age {
        history_age_seconds: 1,
    };
    let sweep = gateway
        .state
        .plan_retention("collect", &policy, 1_000_000_000)
        .unwrap();
    assert!(sweep.records.contains(&SemanticRecordRef::element("b")));
    assert!(!sweep.records.contains(&SemanticRecordRef::element("e17")));
}

#[test]
fn votes_on_a_pending_report_follow_its_source_through_later_revisions() {
    let mut gateway = paper_project();
    let report = contribute(&mut gateway, "author", "e17", "pending", None).unwrap();
    let report = vote(&mut gateway, "v1", &report, true, &["v1"], "pending").unwrap();
    revise_paper(&mut gateway, "edit-1", "v2", false);
    // The edit froze the pending report, so this vote moves it to a successor revision.
    let report = vote(&mut gateway, "v2", &report, true, &["v1", "v2"], "pending").unwrap();
    revise_paper(&mut gateway, "edit-2", "v3", false);
    let report = vote(
        &mut gateway,
        "v3",
        &report,
        true,
        &["v1", "v2", "v3"],
        "approved",
    )
    .unwrap();
    assert_eq!(
        public_reports(&gateway),
        vec![SemanticRecordRef::artifact(&report)]
    );
    let SemanticRevisionBody::Artifact(approved) = gateway
        .state
        .resolve(&SemanticRecordRef::artifact(&report))
        .unwrap()
        .body
    else {
        panic!("artifact")
    };
    assert_eq!(
        approved.semantic_element_id, "e17",
        "every revision of the report keeps the source it was written against"
    );
}

#[test]
fn a_revised_element_that_went_inactive_or_left_the_head_is_refused() {
    let mut gateway = paper_project();
    let receipt = revise_paper(&mut gateway, "edit-1", "v2", false);
    let (folder, proof) = (accepted_id(&receipt, "e17"), accepted_id(&receipt, "pdf17"));
    let pending = contribute(&mut gateway, "author", "e17", "pending", None).unwrap();
    // The owner retires the paper: the folder is revised once more and marked inactive, its proof leaves.
    revise_paper(&mut gateway, "retire", "v3", true);
    let head = gateway.state.selection("working").unwrap();
    assert!(!head.records.contains(&SemanticRecordRef::element(&folder)));

    for source in ["e17", folder.as_str(), "pdf17", proof.as_str()] {
        assert_eq!(
            contribute(&mut gateway, "author", source, "late", None),
            Err(refusal(source)),
            "{source}"
        );
    }
    assert_eq!(
        vote(&mut gateway, "v1", &pending, true, &["v1"], "pending"),
        Err(refusal("e17"))
    );
    contribute(&mut gateway, "author", "b", "unrelated", None).unwrap();
    vote(&mut gateway, "owner", &pending, false, &[], "rejected").unwrap();
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

#[test]
fn a_member_asking_about_the_current_revision_sees_reports_written_against_older_ones() {
    let mut gateway = paper_project();
    contribute(&mut gateway, "author", "e17", "on the adopted folder", None).unwrap();
    let receipt = revise_paper(&mut gateway, "edit-1", "v2", false);
    let folder = accepted_id(&receipt, "e17");
    contribute(
        &mut gateway,
        "author",
        &folder,
        "on the revised folder",
        None,
    )
    .unwrap();
    contribute(&mut gateway, "author", "a", "elsewhere", None).unwrap();
    for (asked, expected) in [
        (
            folder.as_str(),
            vec!["on the adopted folder", "on the revised folder"],
        ),
        // A report follows its element forward; asking about the older revision finds only its own.
        ("e17", vec!["on the adopted folder"]),
    ] {
        let mut store = IndexedGatewayStorage::with_rows(gateway.rows.clone());
        let facts = store.read(
            &gateway.state,
            json!({"operation":"challenge_view","element_ids":[asked]}),
        );
        let view = titles(storage(
            json!({"action":"challenge_display","state_json":serde_json::to_string(&facts).unwrap(),"rows":store.loaded,"public_only":false}),
        ));
        assert_eq!(view, expected, "asked about {asked}");
    }
}

/// The byte requests of one contribution on `element`, then of a pending view asking about `viewed`.
fn requests(gateway: &mut ChallengeGateway, element: &str, viewed: &str) -> Vec<Vec<u8>> {
    let request = gateway.request(
        "author",
        json!({"elementId":element,"title":"review","content":"review"}),
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
            execute(Some(report(&request, element, "review", "pending", None))),
        ]
        .into_iter()
        .map(|request| {
            serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap()
        }),
    );
    contribute(gateway, "author", element, "review", None).unwrap();
    let mut view = IndexedGatewayStorage::with_rows(gateway.rows.clone());
    view.read(
        &gateway.state,
        json!({"operation":"challenge_view","element_ids":[viewed]}),
    );
    requests.extend(view.requests);
    requests
}

pub(super) fn cases() -> Vec<Vec<u8>> {
    let mut cases = requests(&mut ChallengeGateway::with_empty_public_list(), "a", "a");
    // The head holds a successor of `e17`, so the family walk and the older-revision view run as well.
    let mut revised = paper_project();
    let receipt = revise_paper(&mut revised, "edit-1", "v2", false);
    cases.extend(requests(&mut revised, "e17", &accepted_id(&receipt, "e17")));
    cases
}
