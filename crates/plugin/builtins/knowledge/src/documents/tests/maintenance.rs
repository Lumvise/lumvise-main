//! Causal markers describe one accepted body; they never grant read or write access.
use super::*;

#[test]
fn maintenance_marker_preserves_metadata_and_expires_after_an_authored_edit() {
    let mut graph = FakeDocumentGraph::default();
    let mut document = graph
        .call("create_document", draft("chain", "/project"))
        .unwrap()["document"]
        .clone();
    document["metadata"]["maintenance"] = json!({"human":"weekly review"});
    let output = graph
        .call(
            "prepare_document_maintenance",
            json!({"operation":"prepare",
        "document":document,"content":"Maintained","causal_paths":[["a","b"],["c"]]}),
        )
        .unwrap();
    assert_eq!(output["metadata"]["author"], "human");
    assert_eq!(
        output["metadata"]["maintenance"],
        document["metadata"]["maintenance"]
    );
    document["content"] = json!("Maintained");
    document["metadata"] = output["metadata"].clone();
    document["revision"] = json!(2);
    let inspect = |graph: &mut FakeDocumentGraph, document: &Value| {
        graph
            .call(
                "prepare_document_maintenance",
                json!({"operation":"inspect","document":document}),
            )
            .unwrap()["causal_paths"]
            .clone()
    };
    assert_eq!(inspect(&mut graph, &document), json!([["a", "b"], ["c"]]));
    let mut later_revision = document.clone();
    later_revision["revision"] = json!(3);
    assert_eq!(inspect(&mut graph, &later_revision), json!([[]]));
    let mut relocated = document.clone();
    relocated["project_root"] = json!("/other");
    assert_eq!(inspect(&mut graph, &relocated), json!([[]]));
    let mut changed_status = document.clone();
    changed_status["status"] = json!("resolved");
    assert_eq!(inspect(&mut graph, &changed_status), json!([[]]));
    let mut forged = document.clone();
    forged["metadata"]["lumvise_maintenance"]["causal_paths"] = json!([["forged"]]);
    assert_eq!(inspect(&mut graph, &forged), json!([[]]));
    document["content"] = json!("Human change");
    assert_eq!(inspect(&mut graph, &document), json!([[]]));
}

#[test]
fn maintenance_rejects_invalid_paths_and_has_no_storage_effect() {
    let mut graph = FakeDocumentGraph::default();
    let document = graph
        .call("create_document", draft("chain", "/project"))
        .unwrap()["document"]
        .clone();
    let writes = graph.writes;
    for paths in [json!([[""]]), json!([["a", "a"]])] {
        let error = graph
            .call(
                "prepare_document_maintenance",
                json!({"operation":"prepare",
            "document":document,"content":"Maintained","causal_paths":paths}),
            )
            .unwrap_err();
        assert_eq!(error.code, "invalid_maintenance_paths");
    }
    assert_eq!(graph.writes, writes);
}

#[test]
fn maintenance_rejects_an_overflowing_original_revision_without_publication() {
    let mut graph = FakeDocumentGraph::default();
    let mut document = graph
        .call("create_document", draft("chain", "/project"))
        .unwrap()["document"]
        .clone();
    document["revision"] = json!(u64::MAX);
    let writes = graph.writes;
    let error = graph
        .call(
            "prepare_document_maintenance",
            json!({"operation":"prepare",
        "document":document,"content":"Maintained","causal_paths":[["a"]]}),
        )
        .unwrap_err();
    assert_eq!(error.code, "invalid_maintenance_revision");
    assert!(error.message.contains(&u64::MAX.to_string()));
    assert_eq!(graph.writes, writes);
}

#[test]
fn maintenance_refuses_to_replace_a_human_value_in_its_marker_slot() {
    let mut graph = FakeDocumentGraph::default();
    let mut document = graph
        .call("create_document", draft("chain", "/project"))
        .unwrap()["document"]
        .clone();
    document["metadata"]["lumvise_maintenance"] = json!({"human":"Keep this"});
    let error = graph
        .call(
            "prepare_document_maintenance",
            json!({"operation":"prepare",
        "document":document,"content":"Maintained","causal_paths":[["a"]]}),
        )
        .unwrap_err();
    assert_eq!(error.code, "maintenance_marker_conflict");
    assert!(error.message.contains("Keep this"));
}
