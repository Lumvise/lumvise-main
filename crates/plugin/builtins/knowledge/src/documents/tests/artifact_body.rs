//! Locator-and-body documents through the public document exports on the named fake.
//! The body is a staged immutable artifact at the project's document root; the document node
//! holds only a schema-2 locator and its resolved citation edges.

use super::*;
use crate::documents;
use lumvise_plugin_sdk::HostCallTransport;

const BODY: &str = "Body marker 7f3a: the ingestion pipeline is documented here.";

fn document_request(id: &str, root: &str, content: &str) -> Value {
    json!({"document_id": id, "project_root": root, "document_kind": "report",
        "title": "Architecture", "content": content, "path": "Technical/Overview",
        "tags": ["architecture"], "metadata": {"human": {"color": "blue"}}})
}

fn node_id(document_id: &str) -> String {
    crate::documents::records::element_id(document_id)
}

fn seed_source(graph: &mut FakeDocumentGraph, id: &str, root: &str, path: &str) {
    graph.elements.insert(
        id.into(),
        json!({"project_root": root, "semantic_element_id": id,
        "semantic_source_id": "source:files", "path": path, "element_kind": "file",
        "name": path.rsplit('/').next().unwrap_or(path), "parent_element_id": null,
        "content_fingerprint": null, "start_line": 1, "end_line": 3, "lifecycle": "active",
        "metadata": {"provider_instance_id": "filesystem"}}),
    );
}

fn node(graph: &FakeDocumentGraph, document_id: &str) -> Value {
    graph.elements.values().find(|row| {
        row["metadata"]["indexer_metadata"]["knowledge_document"]["document_id"] == document_id
            || row["metadata"]["indexer_metadata"]["knowledge_document_deleted"]["document"]["document_id"] == document_id
    }).unwrap().clone()
}

fn selected_id(graph: &FakeDocumentGraph, document_id: &str) -> String {
    node(graph, document_id)["semantic_element_id"]
        .as_str()
        .unwrap()
        .into()
}

fn locator(graph: &FakeDocumentGraph, document_id: &str) -> Value {
    node(graph, document_id)["metadata"]["indexer_metadata"]["knowledge_document"].clone()
}

fn locator_id(graph: &FakeDocumentGraph, document_id: &str) -> String {
    locator(graph, document_id)["artifact_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn body(graph: &FakeDocumentGraph, artifact_id: &str) -> Value {
    graph.artifacts[artifact_id].clone()
}

fn root_element(graph: &FakeDocumentGraph) -> Value {
    graph
        .elements
        .values()
        .find(|element| element["element_kind"] == "virtual_document_root")
        .unwrap()
        .clone()
}

fn citation_targets(graph: &FakeDocumentGraph, source: &str) -> Vec<String> {
    let mut targets: Vec<String> = graph
        .relationships
        .values()
        .filter(|relationship| relationship["source_element_id"] == source)
        .map(|relationship| {
            relationship["target_element_id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    targets.sort();
    targets
}

fn read(graph: &mut FakeDocumentGraph, document_id: &str) -> Result<Value, PluginError> {
    graph.call("get_document", identity(document_id, "/project"))
}

fn update(document: &Value, patch: Value) -> Value {
    revision_request(document, patch)
}

#[test]
fn the_body_is_staged_under_the_root_and_the_node_holds_only_a_locator() {
    let mut graph = FakeDocumentGraph::default();
    let created = graph
        .call(
            "create_document",
            document_request("overview", "/project", BODY),
        )
        .unwrap()["document"]
        .clone();
    let element = node(&graph, "overview");
    assert_eq!(element["element_kind"], "virtual_document");
    assert_eq!(
        element["path"],
        format!("@documents/{}", hex::encode("overview"))
    );
    assert!(element["parent_element_id"].is_null());
    assert!(!element.to_string().contains("Body marker 7f3a"));
    let pointer = locator(&graph, "overview");
    assert_eq!(pointer.as_object().unwrap().len(), 3);
    assert_eq!(pointer["schema"], 2);
    assert_eq!(pointer["document_id"], "overview");
    let root = root_element(&graph);
    assert_eq!(root["path"], "@documents/root");
    assert_eq!(
        root["metadata"]["indexer_metadata"]["knowledge_document_root"],
        json!({"schema": 1})
    );
    assert!(root["parent_element_id"].is_null());
    let artifact = body(&graph, &locator_id(&graph, "overview"));
    assert_eq!(artifact["artifact_kind"], "knowledge_document");
    assert_eq!(artifact["semantic_element_id"], root["semantic_element_id"]);
    assert_eq!(artifact["content"], BODY);
    let payload = &artifact["metadata"]["knowledge_document"];
    assert_eq!(payload["schema"], 2);
    for field in ["content", "project_root", "semantic_element_id"] {
        assert!(
            payload.get(field).is_none(),
            "{field} must not be duplicated"
        );
    }
    assert_eq!(payload["document_id"], "overview");
    assert_eq!(payload["revision"], 1);
    assert_eq!(read(&mut graph, "overview").unwrap()["document"], created);
}

#[test]
fn publication_accepts_the_body_and_locator_together_after_the_shared_root() {
    let mut graph = FakeDocumentGraph::default();
    let original = graph
        .call(
            "create_document",
            document_request("overview", "/project", BODY),
        )
        .unwrap()["document"]
        .clone();
    assert_eq!(graph.writes, 2);
    assert_eq!(graph.artifact_writes, 1);
    assert_eq!(graph.operations[0], "publish:@documents/root");
    assert_eq!(
        graph.operations[1],
        format!("publish:@documents/{}", hex::encode("overview"))
    );
    graph
        .call(
            "create_document",
            document_request("second", "/project", "Second body"),
        )
        .unwrap();
    assert_eq!((graph.writes, graph.artifact_writes), (3, 2));
    let revised = graph
        .call(
            "update_document",
            update(&original, json!({"content": "# Revised"})),
        )
        .unwrap()["document"]
        .clone();
    assert_eq!((graph.writes, graph.artifact_writes), (4, 3));
    let retained = locator_id(&graph, "overview");
    graph
        .call("delete_document", revision_request(&revised, json!({})))
        .unwrap();
    assert_eq!((graph.writes, graph.artifact_writes), (5, 3));
    assert!(
        node(&graph, "overview")["metadata"]["indexer_metadata"]["knowledge_document_deleted"]
            .is_object()
    );
    assert_eq!(body(&graph, &retained)["content"], "# Revised");
}

#[test]
fn full_body_path_human_metadata_and_revision_checks_round_trip() {
    let mut graph = FakeDocumentGraph::default();
    let original = graph
        .call(
            "create_document",
            document_request("overview", "/project", BODY),
        )
        .unwrap()["document"]
        .clone();
    let updated = graph
        .call(
            "update_document",
            update(
                &original,
                json!({"path": "Decisions", "content": "# Revised body"}),
            ),
        )
        .unwrap()["document"]
        .clone();
    assert_eq!(updated["revision"], 2);
    assert_eq!(updated["path"], "Decisions");
    assert_eq!(updated["tags"], json!(["architecture"]));
    assert_eq!(updated["metadata"], json!({"human": {"color": "blue"}}));
    assert_eq!(read(&mut graph, "overview").unwrap()["document"], updated);
    let artifact = body(&graph, &locator_id(&graph, "overview"));
    assert_eq!(artifact["content"], "# Revised body");
    let payload = &artifact["metadata"]["knowledge_document"];
    assert_eq!(payload["path"], "Decisions");
    assert_eq!(payload["metadata"], updated["metadata"]);
    assert_eq!(payload["revision"], 2);
    let (artifact_writes, publications) = (graph.artifact_writes, graph.writes);
    let stale = graph
        .call(
            "update_document",
            update(&original, json!({"content": "stale"})),
        )
        .unwrap_err();
    assert_eq!(stale.code, "document_revision_conflict");
    assert_eq!(graph.artifact_writes, artifact_writes);
    assert_eq!(graph.writes, publications);
}

#[test]
fn previous_bodies_stay_immutable_and_unlisted_across_revisions() {
    let mut graph = FakeDocumentGraph::default();
    let original = graph
        .call(
            "create_document",
            document_request("overview", "/project", BODY),
        )
        .unwrap()["document"]
        .clone();
    let first = locator_id(&graph, "overview");
    graph
        .call(
            "update_document",
            update(&original, json!({"content": "# Revised"})),
        )
        .unwrap();
    let second = locator_id(&graph, "overview");
    assert_ne!(first, second);
    assert_eq!(body(&graph, &first)["content"], BODY);
    assert_eq!(
        body(&graph, &first)["metadata"]["knowledge_document"]["revision"],
        1
    );
    let listed = graph
        .call("list_knowledge", json!({"project_root": "/project"}))
        .unwrap()
        .to_string();
    assert!(!listed.contains(&first) && !listed.contains(&second));
}

#[test]
fn a_failed_body_stage_keeps_the_previous_revision_and_a_retry_succeeds() {
    let mut graph = FakeDocumentGraph::default();
    let original = graph
        .call(
            "create_document",
            document_request("overview", "/project", BODY),
        )
        .unwrap()["document"]
        .clone();
    let previous = locator_id(&graph, "overview");
    let publications = graph.writes;
    graph.fail_after("artifact", 0);
    let failure = graph
        .call(
            "update_document",
            update(&original, json!({"content": "# Revised"})),
        )
        .unwrap_err();
    assert_eq!(failure.code, INJECTED);
    assert_eq!(locator_id(&graph, "overview"), previous);
    assert_eq!(graph.writes, publications);
    let document = read(&mut graph, "overview").unwrap()["document"].clone();
    assert_eq!(
        (document["revision"].clone(), document["content"].clone()),
        (json!(1), json!(BODY))
    );
    let retried = graph
        .call(
            "update_document",
            update(&original, json!({"content": "# Revised"})),
        )
        .unwrap()["document"]
        .clone();
    assert_eq!(retried["revision"], 2);
    assert_eq!(
        body(&graph, &locator_id(&graph, "overview"))["content"],
        "# Revised"
    );
}

#[test]
fn a_failed_publication_keeps_its_body_private_until_the_frozen_retry_accepts() {
    let mut graph = FakeDocumentGraph::default();
    let original = graph
        .call(
            "create_document",
            document_request("overview", "/project", BODY),
        )
        .unwrap()["document"]
        .clone();
    let previous = locator_id(&graph, "overview");
    let request = update(&original, json!({"content":"# Revised"}));
    graph.fail_after("publication", 0);
    assert_eq!(
        graph
            .call("update_document", request.clone())
            .unwrap_err()
            .code,
        INJECTED
    );
    assert_eq!(locator_id(&graph, "overview"), previous);
    assert_eq!(read(&mut graph, "overview").unwrap()["document"], original);
    assert!(
        graph
            .artifacts
            .values()
            .all(|body| body["content"] != "# Revised")
    );
    let retried = graph.call("update_document", request).unwrap()["document"].clone();
    assert_eq!(retried["revision"], 2);
    assert_eq!(body(&graph, &previous)["content"], BODY);
    assert_eq!(
        graph
            .artifacts
            .values()
            .filter(|body| body["content"] == "# Revised")
            .count(),
        1
    );
    assert_eq!(read(&mut graph, "overview").unwrap()["document"], retried);
}

#[test]
fn a_different_payload_can_accept_without_publishing_the_first_failed_proposal() {
    let mut graph = FakeDocumentGraph::default();
    let original = graph
        .call(
            "create_document",
            document_request("overview", "/project", BODY),
        )
        .unwrap()["document"]
        .clone();
    let first = update(&original, json!({"content":"# First attempt"}));
    graph.fail_after("publication", 0);
    assert_eq!(
        graph
            .call("update_document", first.clone())
            .unwrap_err()
            .code,
        INJECTED
    );
    let accepted = graph
        .call(
            "update_document",
            update(&original, json!({"content":"# Second attempt"})),
        )
        .unwrap()["document"]
        .clone();
    assert!(
        graph
            .artifacts
            .values()
            .all(|body| body["content"] != "# First attempt")
    );
    assert_eq!(
        graph.call("update_document", first).unwrap_err().code,
        "knowledge_revision_conflict"
    );
    assert_eq!(read(&mut graph, "overview").unwrap()["document"], accepted);
}

#[test]
fn a_missing_body_fails_loudly_instead_of_returning_an_empty_document() {
    let mut graph = FakeDocumentGraph::default();
    graph
        .call(
            "create_document",
            document_request("overview", "/project", BODY),
        )
        .unwrap();
    let body_id = locator_id(&graph, "overview");
    graph.artifacts.remove(&body_id);
    graph.revision_artifact_overrides.insert(body_id, None);
    assert!(read(&mut graph, "overview").is_err());
    assert!(
        graph
            .call("list_documents", json!({"project_root": "/project"}))
            .is_err()
    );
}

#[test]
fn equal_public_document_and_artifact_ids_stay_isolated() {
    let mut graph = FakeDocumentGraph::default();
    seed_source(&mut graph, "file:src", "/project", "src/lib.rs");
    graph
        .call(
            "create_document",
            document_request("overview", "/project", BODY),
        )
        .unwrap();
    graph
        .call(
            "create_knowledge",
            json!({"project_root": "/project", "artifact_id": "overview",
                "semantic_element_id": "file:src", "knowledge_type": "specification",
                "title": "Overview requirement", "content": "Requirement text"}),
        )
        .unwrap();
    assert_eq!(
        read(&mut graph, "overview").unwrap()["document"]["title"],
        "Architecture"
    );
    let knowledge = graph
        .call(
            "get_knowledge",
            json!({"project_root": "/project", "artifact_id": "overview"}),
        )
        .unwrap();
    assert_eq!(knowledge["artifact"]["content"], "Requirement text");
}

#[test]
fn resolved_citations_publish_edges_only_to_active_same_project_targets() {
    let mut graph = FakeDocumentGraph::default();
    seed_source(&mut graph, "file:src", "/project", "src/lib.rs");
    seed_source(&mut graph, "file:foreign", "/other", "src/foreign.rs");
    graph
        .call(
            "create_document",
            document_request("target", "/project", "Target body"),
        )
        .unwrap();
    let mut citing = document_request("citing", "/project", BODY);
    citing["references"] = json!([
        {"target_kind": "semantic_element", "semantic_element_id": "file:src"},
        {"target_kind": "semantic_element", "semantic_element_id": "file:foreign"},
        {"target_kind": "semantic_element", "semantic_element_id": "file:missing"},
        {"target_kind": "document", "document_id": "target"},
        {"target_kind": "document", "document_id": "unknown-document"},
        {"target_kind": "artifact", "artifact_id": "overview"}
    ]);
    let created = graph.call("create_document", citing.clone()).unwrap()["document"].clone();
    let mut expected = vec![selected_id(&graph, "target"), "file:src".to_owned()];
    expected.sort();
    assert_eq!(
        citation_targets(&graph, &selected_id(&graph, "citing")),
        expected
    );
    assert_eq!(created["references"], citing["references"]);
    assert_eq!(
        read(&mut graph, "citing").unwrap()["document"]["references"],
        citing["references"]
    );
}

#[test]
fn citation_replacement_and_deletion_keep_the_payload_and_guard_against_seed_revival() {
    let mut graph = FakeDocumentGraph::default();
    seed_source(&mut graph, "file:src", "/project", "src/lib.rs");
    graph
        .call(
            "create_document",
            document_request("target", "/project", "Target body"),
        )
        .unwrap();
    let mut citing = document_request("citing", "/project", BODY);
    citing["references"] = json!([
        {"target_kind": "semantic_element", "semantic_element_id": "file:src"},
        {"target_kind": "document", "document_id": "target"}
    ]);
    let original = graph.call("create_document", citing).unwrap()["document"].clone();
    let replaced = graph
        .call(
            "update_document",
            update(
                &original,
                json!({"references": [{"target_kind": "semantic_element", "semantic_element_id": "file:src"}]}),
            ),
        )
        .unwrap()["document"]
        .clone();
    assert_eq!(
        citation_targets(&graph, &selected_id(&graph, "citing")),
        ["file:src"]
    );
    assert_eq!(replaced["references"].as_array().unwrap().len(), 1);
    let retained = locator_id(&graph, "citing");
    graph
        .call("delete_document", revision_request(&replaced, json!({})))
        .unwrap();
    assert!(citation_targets(&graph, &selected_id(&graph, "citing")).is_empty());
    assert!(
        node(&graph, "citing")["metadata"]["indexer_metadata"]["knowledge_document_deleted"]
            .is_object()
    );
    assert_eq!(body(&graph, &retained)["content"], BODY);
    assert!(read(&mut graph, "citing").is_err());
    let known =
        documents::known_source_documents(&mut PluginContext::for_test(&mut graph), "/project")
            .unwrap();
    assert!(
        known
            .iter()
            .any(|document| document.document_id == "citing")
    );
}

#[test]
fn source_reindex_and_removal_preserve_document_owned_citations_and_the_root() {
    let mut graph = FakeDocumentGraph::default();
    seed_source(&mut graph, "file:src", "/project", "src/lib.rs");
    let mut citing = document_request("citing", "/project", BODY);
    citing["references"] =
        json!([{"target_kind": "semantic_element", "semantic_element_id": "file:src"}]);
    graph.call("create_document", citing).unwrap();
    let root = root_element(&graph)["semantic_element_id"].clone();
    let body_before = locator_id(&graph, "citing");
    let first_base = source_base(&mut graph);
    let mut updated = json!({"provider_instance_id": "filesystem", "project_root": "/project",
        "replace_paths": ["src/lib.rs"], "removed_paths": [], "semantic_sources": [],
        "semantic_elements": [{"semantic_source_id": "source:files", "semantic_element_id": "file:src",
            "path": "src/lib.rs", "semantic_element_type": "file", "semantic_element_name": "lib.rs",
            "parent_element_id": null, "start_line": 1, "end_line": 4,
            "content_fingerprint": "fp1:0000000000000002:v2", "metadata": {}}],
        "semantic_relationships": [], "semantic_artifacts": []});
    updated["revision_observation"] = json!(first_base);
    updated["ingestion_job_id"] = json!("source-edit");
    updated["ingestion_page_index"] = json!(0);
    updated["ingestion_page_count"] = json!(1);
    graph.ingest(&updated).unwrap();
    assert_eq!(
        citation_targets(&graph, &selected_id(&graph, "citing")),
        ["file:src"]
    );
    assert_eq!(locator_id(&graph, "citing"), body_before);
    assert_eq!(root_element(&graph)["semantic_element_id"], root);
    assert_eq!(
        read(&mut graph, "citing").unwrap()["document"]["content"],
        BODY
    );
    let removal_base = source_base(&mut graph);
    graph.ingest(&json!({"provider_instance_id": "filesystem", "project_root": "/project",
            "revision_observation":removal_base, "ingestion_job_id":"source-removal", "ingestion_page_index":0,"ingestion_page_count":1,
            "replace_paths": [], "removed_paths": ["src/lib.rs"], "semantic_sources": [],
            "semantic_elements": [], "semantic_relationships": [], "semantic_artifacts": []}))
        .unwrap();
    // Withdrawing the current source does not retarget the document's exact historical citation.
    let selected = graph.host_call("storage.semantic", json!({"operation":"revision_families", "query":{
        "project_root":"/project", "selection_id":"working", "records":[{"kind":"element","record_id":"file:src"}]}})).unwrap();
    assert_eq!(selected["remaps"], json!([]));
    let document = read(&mut graph, "citing").unwrap()["document"].clone();
    assert_eq!(document["references"][0]["semantic_element_id"], "file:src");
}

fn source_base(graph: &mut FakeDocumentGraph) -> lumvise_contracts::SemanticIndexObservation {
    crate::publication_observation::read(&mut PluginContext::for_test(graph), "/project",
        json!({"kind":"partition", "partition":{"project_root":"/project", "provider_instance_id":"filesystem",
            "replace_paths":["src/lib.rs"]}}), &[]).unwrap()
}

#[test]
fn the_root_marker_is_not_a_parent_or_an_artifact_inheritance_path() {
    let mut graph = FakeDocumentGraph::default();
    seed_source(&mut graph, "file:src", "/project", "src/lib.rs");
    let mut citing = document_request("citing", "/project", BODY);
    citing["references"] =
        json!([{"target_kind": "semantic_element", "semantic_element_id": "file:src"}]);
    graph.call("create_document", citing).unwrap();
    graph
        .call(
            "create_document",
            document_request("other", "/project", "Other body"),
        )
        .unwrap();
    let root = root_element(&graph)["semantic_element_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        graph
            .relationships
            .values()
            .all(|relationship| relationship["relationship_kind"] != "contains")
    );
    assert!(
        graph
            .relationships
            .values()
            .all(|relationship| relationship["source_element_id"] != root.as_str())
    );
    let inherited = graph
        .host_call(
            "storage.semantic",
            json!({"operation": "artifacts_for_elements",
                "semantic_element_ids": [selected_id(&graph, "citing"), selected_id(&graph, "other"), "file:src"]}),
        )
        .unwrap()["artifacts"]
        .clone();
    assert!(
        inherited
            .as_array()
            .unwrap()
            .iter()
            .all(|artifact| artifact["artifact_kind"] != "knowledge_document")
    );
    for artifact in graph
        .artifacts
        .values()
        .filter(|artifact| artifact["artifact_kind"] == "knowledge_document")
    {
        assert_eq!(artifact["semantic_element_id"], root.as_str());
    }
}

#[test]
fn snapshot_loading_excludes_the_root_and_decodes_captured_bodies_without_live_reads() {
    let mut graph = FakeDocumentGraph::default();
    seed_source(&mut graph, "file:src", "/project", "src/lib.rs");
    graph
        .call(
            "create_document",
            document_request("overview", "/project", BODY),
        )
        .unwrap();
    graph.artifact_reads = 0;
    let snapshot = crate::semantic_context::load_document_snapshot(
        &mut PluginContext::for_test(&mut graph),
        "/project",
    )
    .unwrap();
    assert_eq!(snapshot.documents.len(), 1);
    assert_eq!(snapshot.documents[0].content, BODY);
    assert!(snapshot.semantic.elements.iter().all(|element| {
        element.element_kind != "virtual_document_root"
            && element.element_kind != "virtual_document"
    }));
    assert!(
        snapshot
            .semantic
            .elements
            .iter()
            .any(|element| element.semantic_element_id == "file:src")
    );
    assert_eq!(graph.artifact_reads, 0);
}

#[test]
fn old_shape_document_nodes_fail_explicitly_instead_of_being_decoded() {
    let mut graph = FakeDocumentGraph::default();
    graph.elements.insert(
        node_id("legacy"),
        json!({"project_root": "/project", "semantic_element_id": node_id("legacy"),
            "semantic_source_id": "knowledge-documents:2f70726f6a656374",
            "path": format!("@documents/{}", hex::encode("legacy")), "element_kind": "virtual_document",
            "name": "Legacy", "parent_element_id": null, "content_fingerprint": null,
            "start_line": null, "end_line": null, "lifecycle": "active",
            "metadata": {"provider_instance_id": "knowledge.documents", "indexer_metadata": {
                "knowledge_document": {"document_id": "legacy", "document_kind": "report",
                    "title": "Legacy", "content": "Old body", "tags": [], "references": [],
                    "metadata": {}, "status": "active", "revision": 1}}}}),
    );
    let failure = read(&mut graph, "legacy").unwrap_err();
    assert_eq!(failure.code, "invalid_document_record");
    let listing = graph
        .call("list_documents", json!({"project_root": "/project"}))
        .unwrap_err();
    assert_eq!(listing.code, "invalid_document_record");
}

#[test]
fn a_non_document_at_the_document_path_refuses_creation_without_being_touched() {
    let mut graph = FakeDocumentGraph::default();
    let observation = crate::publication_observation::read(
        &mut PluginContext::for_test(&mut graph),
        "/project",
        json!({"kind":"partition", "partition":{"project_root":"/project",
        "provider_instance_id":"filesystem", "replace_paths":[]}}),
        &[],
    )
    .unwrap();
    graph
        .ingest(
            &json!({"provider_instance_id": "filesystem", "project_root": "/project",
            "revision_observation":observation,
            "replace_paths": [], "removed_paths": [], "semantic_sources": [],
            "semantic_elements": [{"semantic_source_id": "source:files",
                "semantic_element_id": node_id("clash"), "path": format!("@documents/{}", hex::encode("clash")),
                "semantic_element_type": "file", "semantic_element_name": "clash.rs",
                "parent_element_id": null, "start_line": 1, "end_line": 2,
                "content_fingerprint": "fp1:0000000000000003:clash", "metadata": {}}],
            "semantic_relationships": [], "semantic_artifacts": []}),
        )
        .unwrap();
    let before = graph.elements[&node_id("clash")].clone();
    let publications = graph.writes;
    let failure = graph
        .call(
            "create_document",
            document_request("clash", "/project", BODY),
        )
        .unwrap_err();
    assert_eq!(failure.code, "document_identity_conflict");
    assert_eq!(graph.elements[&node_id("clash")], before);
    assert_eq!(graph.writes, publications);
    assert!(
        graph
            .artifacts
            .values()
            .all(|artifact| artifact["artifact_kind"] != "knowledge_document")
    );
}
