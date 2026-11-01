//! Ordinary reads use current indexed membership; unrelated heads and retained bodies stay unloaded.
use super::{
    common::*,
    storage_envelopes::{response, storage},
    storage_reads::IndexedGatewayStorage,
};
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
            .iter()
            .any(|forbidden| {
                query["name"] == *forbidden
                    || query["names"]
                        .as_array()
                        .is_some_and(|names| names.iter().any(|name| name == forbidden))
            })
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

/// The queries Rust asks for in its first round of a selected read.
fn first_queries(query: Value) -> Value {
    storage(json!({
        "action":"read",
        "context":{"project_root":ROOT,"project_id":PROJECT_ID,"commit_version":"1"},
        "intent_json":json!({"operation":"selected","selection_id":"working","query":query}).to_string(),
        "rows":[],
        "completed":[],
    }))["queries"]
        .clone()
}

fn read_response(query: Value) -> Value {
    response(json!({
        "action":"read",
        "context":{"project_root":ROOT,"project_id":PROJECT_ID,"commit_version":"1"},
        "intent_json":json!({"operation":"selected","selection_id":"working","query":query}).to_string(),
        "rows":[],
        "completed":[],
    }))
}

#[test]
fn paged_reads_ask_for_one_ordered_stream_and_pass_the_cursor_to_the_adapter() {
    let queries =
        first_queries(json!({"kind":"prefix","prefix":"src/","limit":500,"after":"cursor-1"}));
    assert!(queries.as_array().unwrap().contains(&json!(
        {"query":"prefix","prefix":"src/","selection_id":"working","limit":500,"after":"cursor-1"}
    )));
    let queries = first_queries(
        json!({"kind":"connections","element_ids":["a","b"],"incoming":true,"limit":2}),
    );
    // Both directions of both elements are one stream, so one cursor covers them.
    assert!(queries.as_array().unwrap().contains(&json!(
        {"query":"index_page","names":["connection_source","connection_target"],"values":["a","b"],
         "selection_id":"working","limit":2,"after":null}
    )));
    assert!(!queries.to_string().contains("\"query\":\"index\""));
    let queries = first_queries(
        json!({"kind":"artifacts_by_namespace","artifact_namespace":"knowledge","limit":1000,"after":"c"}),
    );
    assert!(queries.as_array().unwrap().contains(&json!(
        {"query":"index_page","names":["artifact_namespace"],"values":["knowledge"],
         "selection_id":"working","limit":1000,"after":"c"}
    )));
    let queries = first_queries(json!({"kind":"element_roots","limit":500}));
    assert!(queries.as_array().unwrap().contains(&json!(
        {"query":"index_page","names":["element_root"],"values":["root"],
         "selection_id":"working","limit":500,"after":null}
    )));
}

#[test]
fn reads_without_a_limit_ask_exactly_what_they_always_asked() {
    let queries = first_queries(json!({"kind":"prefix","prefix":"src/"}));
    assert!(queries.as_array().unwrap().contains(&json!(
        {"query":"prefix","prefix":"src/","selection_id":"working"}
    )));
    let queries = first_queries(json!({"kind":"connections","element_ids":["a"],"incoming":true}));
    for name in ["connection_source", "connection_target"] {
        assert!(queries.as_array().unwrap().contains(&json!(
            {"query":"index","name":name,"value":"a","selection_id":"working"}
        )));
    }
    let queries =
        first_queries(json!({"kind":"artifacts_by_namespace","artifact_namespace":"knowledge"}));
    assert!(queries.as_array().unwrap().contains(&json!(
        {"query":"index","name":"artifact_namespace","value":"knowledge","selection_id":"working"}
    )));
    let queries = first_queries(json!({"kind":"element_roots"}));
    assert!(queries.as_array().unwrap().contains(&json!(
        {"query":"index","name":"element_root","value":"root","selection_id":"working"}
    )));
}

#[test]
fn a_limit_outside_1_to_1000_and_a_cursor_without_a_limit_are_refused() {
    for (query, expected) in [
        (
            json!({"kind":"prefix","prefix":"src/","limit":0}),
            "a page limit from 1 to 1,000",
        ),
        (
            json!({"kind":"connections","element_ids":["a"],"incoming":false,"limit":1001}),
            "a page limit from 1 to 1,000",
        ),
        (
            json!({"kind":"artifacts_by_namespace","artifact_namespace":"knowledge","limit":1001}),
            "a page limit from 1 to 1,000",
        ),
        (
            json!({"kind":"prefix","prefix":"src/","after":"cursor"}),
            "a page limit together with the cursor",
        ),
        (
            json!({"kind":"element_roots","limit":0}),
            "a page limit from 1 to 1,000",
        ),
        (
            json!({"kind":"element_roots","after":"cursor"}),
            "a page limit together with the cursor",
        ),
    ] {
        let response = read_response(query.clone());
        assert_eq!(response["status"], "rejected", "{query}");
        assert!(
            response["message"].as_str().unwrap().contains(expected),
            "{query}: {response}"
        );
    }
    // 1 and 1,000 are the ends of the range.
    for limit in [1, 1000] {
        let queries = first_queries(json!({"kind":"prefix","prefix":"src/","limit":limit}));
        let page = queries
            .as_array()
            .unwrap()
            .iter()
            .find(|query| query["query"] == "prefix")
            .expect("prefix page query");
        assert_eq!(page["limit"], limit);
    }
}

#[test]
fn an_artifact_gets_one_namespace_entry_for_each_metadata_key_that_holds_an_object() {
    let namespaces = |rows: &Value, id: &str| {
        let mut found = rows
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| {
                row["key"]["kind"] == "record"
                    && row["key"]["identity"]
                        == serde_json::to_string(&SemanticRecordRef::artifact(id)).unwrap()
            })
            .flat_map(|row| row["indexes"].as_array().unwrap().clone())
            .filter(|index| index["name"] == "artifact_namespace")
            .map(|index| index["value"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        found.sort();
        found
    };
    let mut note = artifact("note", "a", "note");
    note.metadata =
        json!({"knowledge":{"kind":"note"},"reports":{},"title":"plain","count":3,"tags":["x"]});
    let mut plain = artifact("plain", "a", "plain");
    plain.metadata = json!({"title":"nothing here"});
    // Publication writes them.
    let mut project = ProjectFixture::new();
    project.initial();
    let mut edit = project.edit("artifacts", vec![]);
    edit.structure = None;
    for artifact in [&note, &plain] {
        edit.artifacts.push(SemanticArtifactRevisionWrite {
            checkpoint: false,
            artifact: artifact.clone(),
            media_type: "text/plain".into(),
        });
    }
    project.publish(&edit);
    let rows = storage(
        json!({"action":"rows","state_json":serde_json::to_string(&project.state).unwrap()}),
    );
    assert_eq!(namespaces(&rows, "note"), ["knowledge", "reports"]);
    assert_eq!(namespaces(&rows, "plain"), Vec::<String>::new());
    // Archive adoption writes the same entries.
    let adopted = SemanticRevisionState::adopt(
        ROOT,
        PROJECT_ID,
        "working",
        "local",
        vec![element("a", "a.rs", "body")],
        vec![note, plain],
        vec![],
        1,
        1,
    )
    .unwrap();
    let rows =
        storage(json!({"action":"rows","state_json":serde_json::to_string(&adopted).unwrap()}));
    assert_eq!(namespaces(&rows, "note"), ["knowledge", "reports"]);
    assert_eq!(namespaces(&rows, "plain"), Vec::<String>::new());
}

#[test]
fn a_namespace_read_loads_the_selected_artifacts_of_that_namespace_only() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut edit = project.edit("artifacts", vec![]);
    edit.structure = None;
    for (id, namespace) in [("k1", "knowledge"), ("k2", "knowledge"), ("r1", "reports")] {
        let mut artifact = artifact(id, "a", id);
        artifact.metadata = json!({ namespace: {"kind":"note"} });
        edit.artifacts.push(SemanticArtifactRevisionWrite {
            checkpoint: false,
            artifact,
            media_type: "text/plain".into(),
        });
    }
    project.publish(&edit);
    for query in [
        json!({"kind":"artifacts_by_namespace","artifact_namespace":"knowledge"}),
        json!({"kind":"artifacts_by_namespace","artifact_namespace":"knowledge","limit":10}),
    ] {
        let (facts, _) = selected_facts(&project, "working", query);
        let mut ids = facts
            .snapshot("working", "now")
            .unwrap()
            .unwrap()
            .artifacts
            .iter()
            .map(|artifact| artifact.artifact_id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        assert_eq!(ids, ["k1", "k2"]);
    }
}

fn tree() -> Vec<SemanticElement> {
    let mut inner = element("inner", "inner.rs", "inner");
    inner.parent_element_id = Some("top".into());
    let mut dormant = element("dormant", "dormant.rs", "dormant");
    dormant.lifecycle = "inactive".into();
    vec![
        element("top", "top.rs", "top"),
        element("also-top", "also.rs", "also"),
        inner,
        dormant,
    ]
}

#[test]
fn an_active_element_without_a_parent_gets_the_root_entry_by_every_write_path() {
    let roots = |rows: &Value| {
        let mut found =
            rows.as_array()
                .unwrap()
                .iter()
                .filter(|row| {
                    row["key"]["kind"] == "record"
                        && row["indexes"].as_array().unwrap().iter().any(|index| {
                            index["name"] == "element_root" && index["value"] == "root"
                        })
                })
                .map(|row| {
                    serde_json::from_str::<Value>(row["key"]["identity"].as_str().unwrap()).unwrap()
                        ["record_id"]
                        .as_str()
                        .unwrap()
                        .to_string()
                })
                .collect::<Vec<_>>();
        found.sort();
        found
    };
    let mut project = ProjectFixture::new();
    project.publish(&project.edit("tree", tree()));
    let rows = storage(
        json!({"action":"rows","state_json":serde_json::to_string(&project.state).unwrap()}),
    );
    // The child has a parent and the dormant element is not active: neither is a root.
    assert_eq!(roots(&rows), ["also-top", "top"]);
    // Archive adoption writes the same entries.
    let adopted = SemanticRevisionState::adopt(
        ROOT,
        PROJECT_ID,
        "working",
        "local",
        tree(),
        vec![],
        vec![],
        1,
        1,
    )
    .unwrap();
    let rows =
        storage(json!({"action":"rows","state_json":serde_json::to_string(&adopted).unwrap()}));
    assert_eq!(roots(&rows), ["also-top", "top"]);
}

#[test]
fn a_roots_read_loads_the_selected_active_roots_and_follows_revisions() {
    let ids = |project: &ProjectFixture, query: Value| {
        let (facts, _) = selected_facts(project, "working", query);
        let mut ids = facts
            .snapshot("working", "now")
            .unwrap()
            .unwrap()
            .elements
            .iter()
            .map(|element| element.semantic_element_id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids
    };
    let mut project = ProjectFixture::new();
    project.publish(&project.edit("tree", tree()));
    for query in [
        json!({"kind":"element_roots"}),
        json!({"kind":"element_roots","limit":10}),
    ] {
        assert_eq!(ids(&project, query), ["also-top", "top"]);
    }
    // A root that gains a parent stops being one. The edit repeats the others, as a whole-project edit does.
    let mut regrouped = tree();
    regrouped[1].parent_element_id = Some("top".into());
    project.publish(&project.edit("regroup", regrouped.clone()));
    assert_eq!(ids(&project, json!({"kind":"element_roots"})), ["top"]);
    // A revised root is read as its new revision, and the old one is not read at all.
    regrouped[0].metadata = json!({"body":"top, revised"});
    let receipt = project.publish(&project.edit("revise", regrouped));
    assert_eq!(
        ids(&project, json!({"kind":"element_roots","limit":10})),
        [accepted_id(&receipt, "top")]
    );
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
