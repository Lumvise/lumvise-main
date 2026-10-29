//! Ordinary reads use current indexed membership; unrelated heads and retained bodies stay unloaded.
use super::{common::*, storage_reads::IndexedGatewayStorage};
use lumvise_semantic_revisions::*;
use serde_json::{Value, json};

fn selected_facts(
    project: &ProjectFixture,
    selection: &str,
    query: Value,
) -> (SemanticRevisionState, IndexedGatewayStorage) {
    let mut store = IndexedGatewayStorage::new(&project.state);
    let facts = store.read(
        &project.state,
        json!({"operation":"selected","selection_id":selection,"query":query}),
    );
    assert!(!store.queries.iter().any(|query| {
        query["query"] == "all"
            || [
                "selection_member",
                "member_record",
                "follow_context",
                "history_selection",
            ]
            .contains(&query["name"].as_str().unwrap_or(""))
    }));
    assert!(
        facts
            .selections
            .iter()
            .all(|head| head.selection.selection_id == selection)
    );
    (facts, store)
}

#[test]
fn exact_and_path_reads_fetch_current_matches_without_historical_bodies() {
    let mut project = ProjectFixture::new();
    project.initial();
    let change = project.edit(
        "advance",
        vec![element("a", "a.rs", "new"), element("b", "b.rs", "B1")],
    );
    let receipt = project.publish(&change);
    let newest = accepted_id(&receipt, "a");
    for query in [
        json!({"kind":"records","records":[SemanticRecordRef::element("a"),SemanticRecordRef::element(&newest)]}),
        json!({"kind":"path","path":"a.rs"}),
        json!({"kind":"prefix","prefix":"a."}),
    ] {
        let (facts, _) = selected_facts(&project, "working", query);
        assert_eq!(facts.records.len(), 1);
        assert_eq!(facts.records[0].record.record_id, newest);
        assert_eq!(
            facts
                .snapshot("working", "now")
                .unwrap()
                .unwrap()
                .elements
                .len(),
            1
        );
    }
}

#[test]
fn location_reads_load_selected_containment_ancestors_without_unrelated_records() {
    for (kind, label) in [("contains", "folder"), ("semantic", "contains")] {
        let mut project = ProjectFixture::new();
        let mut edit = project.edit(
            "initial",
            vec![
                element("root", ".", "root"),
                element("folder", "src", "folder"),
                element("a", "src/a.rs", "a"),
                element("other", "other.rs", "other"),
            ],
        );
        edit.structure.as_mut().unwrap().relationships = [("root", "folder"), ("folder", "a")]
            .into_iter()
            .map(|(source, target)| SemanticRelationship {
                project_root: ROOT.into(),
                source_element_id: source.into(),
                target_element_id: target.into(),
                relationship_kind: kind.into(),
                label: label.into(),
                metadata: json!({}),
            })
            .collect();
        project.publish(&edit);
        let (facts, _) = selected_facts(
            &project,
            "working",
            json!({"kind":"path","path":"src/a.rs"}),
        );
        let snapshot = facts.snapshot("working", "now").unwrap().unwrap();
        assert_eq!(snapshot.elements.len(), 3);
        assert_eq!(snapshot.connections.len(), 2);
        assert!(
            !facts
                .records
                .iter()
                .any(|record| record.record.record_id == "other")
        );
    }
}

#[test]
fn inherited_owner_reads_keep_contextual_attachment_without_loading_the_old_owner() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut note = project.edit("note", vec![]);
    note.structure = None;
    note.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: artifact("note", "a", "retained"),
        media_type: "text/plain".into(),
    });
    project.publish(&note);
    let change = project.edit(
        "advance",
        vec![element("a", "a.rs", "new"), element("b", "b.rs", "B1")],
    );
    let receipt = project.publish(&change);
    let newest = accepted_id(&receipt, "a");
    let (facts, _) = selected_facts(
        &project,
        "working",
        json!({"kind":"owners","element_ids":[newest]}),
    );
    let snapshot = facts.snapshot("working", "now").unwrap().unwrap();
    assert_eq!(snapshot.artifacts.len(), 1);
    assert_eq!(snapshot.artifacts[0].artifact_id, "note");
    assert_eq!(snapshot.associations.len(), 1);
    assert_eq!(snapshot.associations[0].semantic_element_id, newest);
    assert!(
        facts
            .records
            .iter()
            .all(|record| !["a", "b"].contains(&record.record.record_id.as_str()))
    );
    let (old, _) = selected_facts(
        &project,
        "working",
        json!({"kind":"owners","element_ids":["a"]}),
    );
    assert!(old.records.is_empty());
}

#[test]
fn public_selected_read_keeps_the_shared_explicit_dependency_gate() {
    let mut project = ProjectFixture::new();
    let mut dependent = element("a", "a.rs", "needs b");
    dependent
        .revision_dependencies
        .push(SemanticRecordRef::element("b"));
    project.publish(&project.edit("seed", vec![dependent, element("b", "b.rs", "private")]));
    let mut published = project.state.selection("working").unwrap().clone();
    published.selection.selection_id = "public".into();
    published.selection.mode = SemanticSelectionMode::Pinned;
    published.records.retain(|record| record.record_id == "a");
    project.state.selections.push(published);
    let (facts, _) = selected_facts(
        &project,
        "public",
        json!({"kind":"records","records":[SemanticRecordRef::element("a")]}),
    );
    assert!(
        facts
            .snapshot("public", "now")
            .unwrap_err()
            .to_string()
            .contains("explicitly published dependency")
    );
    let (private, _) = selected_facts(
        &project,
        "public",
        json!({"kind":"records","records":[SemanticRecordRef::element("b")]}),
    );
    assert!(private.records.is_empty());
}

pub(super) fn cases() -> Vec<Vec<u8>> {
    let mut project = ProjectFixture::new();
    project.initial();
    let query = json!({"kind":"prefix","prefix":"a."});
    let (facts, store) = selected_facts(&project, "working", query.clone());
    let mut requests = store.requests;
    let request = json!({"operation":"selected","selection_id":"working","query":query});
    requests.push(serde_json::to_vec(&json!({"operation":"storage","request":{"action":"execute","rows":[],"request_json":request.to_string(),"state_json":serde_json::to_string(&facts).unwrap(),"authenticated_actor":null,"now_ms":"2000","published_at":"time","next_commit":"9007199254741007"}})).unwrap());
    requests.push(
        serde_json::to_vec(&json!({"operation":"storage","request":header_request()})).unwrap(),
    );
    requests
}

fn header_request() -> Value {
    let mut project = ProjectFixture::new();
    let mut pdf = element("pdf\0id", "papers/é.PDF", &"large text".repeat(1000));
    pdf.parent_element_id = Some("folder".into());
    project.publish(&project.edit("pdf", vec![element("folder", "papers", "folder"), pdf]));
    let rows = super::storage_envelopes::storage(
        json!({"action":"rows","state_json":serde_json::to_string(&project.state).unwrap()}),
    );
    let selected = rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| {
            row["indexes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|index| index["name"] == "element_extension" && index["value"] == "pdf")
        })
        .cloned()
        .map(|mut row| {
            row["body_json"] = Value::Null;
            row
        })
        .collect::<Vec<_>>();
    json!({"action":"element_headers","rows":selected})
}

#[test]
fn body_free_element_headers_preserve_paths_and_reject_forged_identity() {
    let request = header_request();
    let result = super::storage_envelopes::storage(request.clone());
    let projected: Value = serde_json::from_str(result["value_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        projected["elements"],
        json!([{"semantic_element_id":"pdf\0id","name":"pdf\0id","lifecycle":"active","path":"papers/é.PDF","parent_element_id":"folder"}])
    );
    assert!(!result.to_string().contains("large text"));
    let mut forged = request;
    forged["rows"][0]["key"]["identity"] = json!("foreign");
    let response: Value = serde_json::from_slice(&evaluate_revision_policy(
        &serde_json::to_vec(&json!({"operation":"storage","request":forged})).unwrap(),
    ))
    .unwrap();
    assert_eq!(response["status"], "rejected");
}
