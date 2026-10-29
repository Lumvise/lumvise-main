//! Locator-and-body documents on the real local persistence: reopen, interrupted writes, and PZ transfer.
//! Each test uses the public document exports and reads the stored records back through Semantic.

use super::*;
use lumvise_db_core::{ProjectSnapshotScope, SemanticProjectSnapshot};

const BODY: &str = "Real body marker 9c21: pipeline notes.";

fn request(id: &str, content: &str) -> Value {
    json!({"document_id": id, "project_root": "/project", "document_kind": "report",
        "title": "Architecture", "content": content, "path": "Technical/Overview",
        "tags": ["architecture"], "metadata": {"human": {"color": "blue"}}})
}

fn identity(id: &str, root: &str) -> Value {
    json!({"document_id": id, "project_root": root})
}

fn snapshot(host: &DocumentBoundaryHost, root: &str) -> SemanticProjectSnapshot {
    match host.execute(SemanticOperation::ProjectSnapshot {
        scope: ProjectSnapshotScope::ProjectRoot(root.into()),
        artifact_namespace: None,
    }) {
        SemanticResult::ProjectSnapshot(snapshot) => snapshot,
        other => panic!("expected project snapshot, got {other:?}"),
    }
}

fn node_locator(captured: &SemanticProjectSnapshot, document_id: &str) -> Value {
    let element = captured
        .elements
        .iter()
        .find(|element| {
            element.metadata["indexer_metadata"]["knowledge_document"]["document_id"] == document_id
        })
        .expect("document node");
    element.metadata["indexer_metadata"]["knowledge_document"].clone()
}

fn stored_body(
    captured: &SemanticProjectSnapshot,
    document_id: &str,
) -> lumvise_db_core::SemanticArtifact {
    let artifact_id = node_locator(captured, document_id)["artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    captured
        .artifacts
        .iter()
        .find(|artifact| artifact.artifact_id == artifact_id)
        .expect("stored body artifact")
        .clone()
}

fn read(host: &mut DocumentBoundaryHost, id: &str) -> Result<Value, PluginError> {
    host.knowledge("get_document", identity(id, "/project"))
}

#[test]
fn a_body_survives_reopen_and_the_node_keeps_only_a_locator() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("documents.sqlite");
    let mut host =
        DocumentBoundaryHost::with_persistence(Arc::new(LocalPersistence::open(&path).unwrap()));
    let created = host
        .knowledge("create_document", request("report:architecture", BODY))
        .unwrap();
    drop(host);
    let mut reopened =
        DocumentBoundaryHost::with_persistence(Arc::new(LocalPersistence::open(&path).unwrap()));
    assert_eq!(read(&mut reopened, "report:architecture").unwrap(), created);
    let captured = snapshot(&reopened, "/project");
    let node = captured
        .elements
        .iter()
        .find(|element| element.element_kind == "virtual_document")
        .unwrap();
    assert!(
        !serde_json::to_string(node)
            .unwrap()
            .contains("Real body marker")
    );
    let root = captured
        .elements
        .iter()
        .find(|element| element.element_kind == "virtual_document_root")
        .unwrap();
    let body = stored_body(&captured, "report:architecture");
    assert_eq!(body.content.as_deref(), Some(BODY));
    assert_eq!(body.semantic_element_id, root.semantic_element_id);
    assert_eq!(body.artifact_kind, "knowledge_document");
}

#[test]
fn the_body_and_pointer_are_published_together_after_the_root_on_the_real_boundary() {
    let mut host = DocumentBoundaryHost::new();
    let created = host
        .knowledge("create_document", request("report:architecture", BODY))
        .unwrap();
    assert_eq!(host.writes, ["publish", "publish"]);
    host.writes.clear();
    host.knowledge(
        "update_document",
        json!({"document_id": "report:architecture", "project_root": "/project",
            "expected_revision": 1, "expected_semantic_element_id":created["document"]["semantic_element_id"], "content": "# Revised"}),
    )
    .unwrap();
    assert_eq!(host.writes, ["publish"]);
}

#[test]
fn a_failed_body_stage_publishes_no_pointer_and_a_retry_succeeds() {
    let mut host = DocumentBoundaryHost::new();
    host.fail_document_write = Some(1);
    let failure = host
        .knowledge("create_document", request("report:architecture", BODY))
        .unwrap_err();
    assert_eq!(failure.code, "injected_write_failure");
    assert_eq!(
        read(&mut host, "report:architecture").unwrap_err().code,
        "document_not_found"
    );
    assert!(snapshot(&host, "/project").artifacts.is_empty());
    host.fail_document_write = None;
    let created = host
        .knowledge("create_document", request("report:architecture", BODY))
        .unwrap();
    assert_eq!(read(&mut host, "report:architecture").unwrap(), created);
}

#[test]
fn a_failed_pointer_publication_keeps_the_previous_body_and_a_retry_succeeds() {
    let mut host = DocumentBoundaryHost::new();
    let created = host
        .knowledge("create_document", request("report:architecture", BODY))
        .unwrap();
    host.fail_document_write = Some(0);
    let failure = host
        .knowledge(
            "update_document",
            json!({"document_id": "report:architecture", "project_root": "/project",
                "expected_revision": 1, "expected_semantic_element_id":created["document"]["semantic_element_id"], "content": "# Revised"}),
        )
        .unwrap_err();
    assert_eq!(failure.code, "injected_write_failure");
    assert_eq!(read(&mut host, "report:architecture").unwrap(), created);
    host.fail_document_write = None;
    let retried = host
        .knowledge(
            "update_document",
            json!({"document_id": "report:architecture", "project_root": "/project",
                "expected_revision": 1, "expected_semantic_element_id":created["document"]["semantic_element_id"], "content": "# Revised"}),
        )
        .unwrap();
    assert_eq!(retried["document"]["revision"], 2);
    assert_eq!(
        read(&mut host, "report:architecture").unwrap()["document"]["content"],
        "# Revised"
    );
}

#[test]
fn a_missing_stored_body_fails_the_read_on_the_real_boundary() {
    let mut host = DocumentBoundaryHost::new();
    host.knowledge("create_document", request("report:architecture", BODY))
        .unwrap();
    let captured = snapshot(&host, "/project");
    let body_id = node_locator(&captured, "report:architecture")["artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    host.missing_body = Some(body_id);
    assert_eq!(
        read(&mut host, "report:architecture").unwrap_err().code,
        "document_revision_unavailable"
    );
}

#[test]
fn archive_export_and_import_keep_body_path_and_assets_under_the_imported_root() {
    let directory = tempfile::tempdir().unwrap();
    let archive_path = directory.path().join("project.pz");
    let mut source = DocumentBoundaryHost::new();
    source
        .knowledge(
            "create_document",
            json!({"document_id": "board:architecture", "project_root": "/project",
                "document_kind": "canvas", "title": "Architecture board", "content": "{\"nodes\":[]}",
                "path": "Technical/Boards", "tags": ["boards"], "metadata": {"human": {"color": "blue"}}}),
        )
        .unwrap();
    let original = source
        .knowledge("get_document", identity("board:architecture", "/project"))
        .unwrap()["document"]
        .clone();
    let owner = original["attachment_owner_id"].as_str().unwrap().to_owned();
    let SemanticResult::ProjectArchive(archive) =
        source.execute(SemanticOperation::ProjectArchive {
            project_root: "/project".into(),
        })
    else {
        panic!("expected project archive");
    };
    archive
        .write(&archive_path, &InvocationControl::sixty_seconds())
        .unwrap();
    let target = Arc::new(LocalPersistence::in_memory().unwrap());
    SemanticPersistence::execute(
        target.as_ref(),
        SemanticOperation::ImportPzSnapshot {
            project_root: "/imported".into(),
            input_path: archive_path.to_string_lossy().into_owned(),
        },
        &InvocationControl::sixty_seconds(),
    )
    .unwrap();
    let mut imported = DocumentBoundaryHost::with_persistence(target);
    let document = imported
        .knowledge("get_document", identity("board:architecture", "/imported"))
        .unwrap()["document"]
        .clone();
    let mut expected = original.clone();
    expected["project_root"] = json!("/imported");
    assert_eq!(document, expected);
    let captured = snapshot(&imported, "/imported");
    let root = captured
        .elements
        .iter()
        .find(|element| element.element_kind == "virtual_document_root")
        .unwrap();
    let body = stored_body(&captured, "board:architecture");
    assert_eq!(body.semantic_element_id, root.semantic_element_id);
    assert!(
        captured
            .artifacts
            .iter()
            .any(|artifact| artifact.artifact_id == owner)
    );
}
