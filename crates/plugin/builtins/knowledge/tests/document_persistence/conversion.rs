use super::*;

fn seeded() -> DocumentBoundaryHost {
    let host = DocumentBoundaryHost::new();
    host.semantic("ingest_index_batch", source_batch()).unwrap();
    host
}

fn legacy(
    host: &mut DocumentBoundaryHost,
    id: &str,
    kind: &str,
    content: &str,
    tags: &[&str],
) -> Value {
    host.knowledge("create_knowledge", json!({"artifact_id": id, "semantic_element_id": "file:lib",
        "knowledge_type": kind, "title": id, "content": content, "tags": tags,
        "project_root": "/project", "path": "Architecture/Decisions", "metadata": {"human": "kept", "canvas": {"parentCanvasId": "parent"}}})).unwrap()["artifact"].clone()
}

fn convert(host: &mut DocumentBoundaryHost, preview: bool) -> Result<Value, PluginError> {
    host.knowledge(
        "convert_legacy_documents",
        json!({"project_root": "/project", "preview": preview}),
    )
}

#[test]
fn workflow_report_tags_preserve_browsable_reports_across_older_storage_kinds() {
    let mut host = seeded();
    legacy(
        &mut host,
        "living",
        "specification",
        "Human report",
        &["living-report"],
    );
    legacy(
        &mut host,
        "diagram",
        "definition",
        "Architecture",
        &["scoped-c4"],
    );
    legacy(
        &mut host,
        "functional",
        "report",
        "Source context",
        &["living-report", "cultivation-functional"],
    );
    convert(&mut host, false).unwrap();
    assert_eq!(document(&mut host, "living")["document_kind"], "report");
    assert_eq!(document(&mut host, "diagram")["document_kind"], "report");
    assert_eq!(
        document(&mut host, "functional")["document_kind"],
        "context"
    );
    assert_eq!(document(&mut host, "living")["content"], "Human report");
}

fn document(host: &mut DocumentBoundaryHost, id: &str) -> Value {
    host.knowledge(
        "get_document",
        json!({"project_root": "/project", "document_id": id}),
    )
    .unwrap()["document"]
        .clone()
}

#[test]
fn ruleset_provenance_keeps_the_original_content_digest_after_a_human_edit() {
    use sha2::{Digest, Sha256};
    let mut host = seeded();
    legacy(
        &mut host,
        "ruleset:digest",
        "specification",
        "Original executed rules",
        &["agents", "ruleset"],
    );
    let digest = format!("{:x}", Sha256::digest(b"Original executed rules"));
    let preview = convert(&mut host, true).unwrap();
    assert_eq!(preview["conversions"][0]["source_content_sha256"], digest);
    assert_eq!(
        preview["conversions"][0]["source_semantic_element_id"],
        "file:lib"
    );
    convert(&mut host, false).unwrap();
    let ruleset = document(&mut host, "ruleset:digest");
    let edited = host.knowledge("update_document", json!({"project_root":"/project",
        "document_id":"ruleset:digest", "expected_revision":ruleset["revision"], "content":"Human's new instructions"})).unwrap();
    assert_eq!(
        edited["document"]["metadata"]["legacy_knowledge"]["content_sha256"],
        digest
    );
    assert_eq!(edited["document"]["content"], "Human's new instructions");
}

fn revision(host: &DocumentBoundaryHost) -> i64 {
    let SemanticResult::SemanticRevision { commit_version } =
        host.execute(SemanticOperation::SemanticRevision)
    else {
        panic!("semantic revision expected")
    };
    commit_version
}

#[test]
fn conversion_preview_is_read_only_and_retains_the_artifact_document_distinction() {
    let mut host = seeded();
    let report = legacy(&mut host, "report:a", "report", "# Human report", &[]);
    legacy(
        &mut host,
        "task:a",
        "task_assignment",
        "A future task is context, not a mission",
        &[],
    );
    legacy(
        &mut host,
        "spec:a",
        "specification",
        "Keep the specification",
        &[],
    );
    let revision = revision(&host);
    let preview = host
        .knowledge(
            "convert_legacy_documents",
            json!({"project_root": "/project"}),
        )
        .unwrap();
    assert_eq!(preview["preview"], true);
    assert_eq!(preview["total"], 2);
    assert_eq!(self::revision(&host), revision);
    assert_eq!(
        host.knowledge("get_knowledge", json!({"artifact_id": "report:a"}))
            .unwrap()["artifact"],
        report
    );
    assert_eq!(
        host.knowledge("list_documents", json!({"project_root": "/project"}))
            .unwrap()["total"],
        0
    );
}

#[test]
fn conversion_preserves_lifecycle_metadata_source_and_links_without_creating_missions() {
    let mut host = seeded();
    legacy(&mut host, "report:a", "report", "# Human report", &[]);
    let archived = host
        .knowledge(
            "update_knowledge",
            json!({"artifact_id": "report:a", "status": "archived"}),
        )
        .unwrap()["artifact"]
        .clone();
    legacy(
        &mut host,
        "task:a",
        "task_assignment",
        "[Report](lumvise://artifact/report%3Aa) [Other](lumvise://artifact/report%3Aa-more)",
        &[],
    );
    convert(&mut host, false).unwrap();
    let report = document(&mut host, "report:a");
    for field in [
        "content",
        "status",
        "revision",
        "created_at",
        "updated_at",
        "path",
        "tags",
    ] {
        assert_eq!(report[field], archived[field], "preserve {field}");
    }
    assert_eq!(report["metadata"]["human"], "kept");
    assert_eq!(report["references"][0]["semantic_element_id"], "file:lib");
    let task = document(&mut host, "task:a");
    assert_eq!(task["document_kind"], "context");
    assert_eq!(
        task["content"],
        "[Report](lumvise://document/report%3Aa) [Other](lumvise://artifact/report%3Aa-more)"
    );
    assert_eq!(
        host.knowledge("list_knowledge", json!({})).unwrap()["artifacts"],
        json!([])
    );
    assert_eq!(convert(&mut host, false).unwrap()["total"], 0);
}

#[test]
fn an_existing_document_collision_stops_before_any_legacy_record_is_retired() {
    let mut host = seeded();
    legacy(
        &mut host,
        "report:architecture",
        "report",
        "Legacy text",
        &[],
    );
    host.knowledge("create_document", report()).unwrap();
    let revision = revision(&host);
    let error = convert(&mut host, false).unwrap_err();
    assert_eq!(error.code, "document_conversion_conflict");
    assert_eq!(self::revision(&host), revision);
    assert_eq!(
        document(&mut host, "report:architecture")["content"],
        "# A standalone report"
    );
}

#[test]
fn retry_after_interruption_preserves_human_edits_to_an_already_staged_document() {
    let mut host = seeded();
    legacy(&mut host, "report:a", "report", "First", &[]);
    legacy(&mut host, "report:b", "report", "Second", &[]);
    host.fail_document_write = Some(1);
    assert_eq!(
        convert(&mut host, false).unwrap_err().code,
        "injected_write_failure"
    );
    let listed = host
        .knowledge("list_documents", json!({"project_root": "/project"}))
        .unwrap();
    assert_eq!(listed["total"], 1);
    let staged = &listed["documents"][0];
    host.knowledge(
        "update_document",
        json!({"document_id": staged["document_id"], "project_root": "/project",
        "expected_revision": staged["revision"], "content": "Human edits after interruption"}),
    )
    .unwrap();
    let retried = convert(&mut host, false).unwrap();
    assert_eq!(retried["total"], 2);
    assert_eq!(
        document(&mut host, staged["document_id"].as_str().unwrap())["content"],
        "Human edits after interruption"
    );
    assert_eq!(convert(&mut host, false).unwrap()["total"], 0);
}

#[test]
fn a_changed_legacy_record_during_an_interrupted_conversion_requires_review() {
    let mut host = seeded();
    legacy(&mut host, "report:a", "report", "First", &[]);
    host.fail_resource_write = Some(0);
    assert_eq!(
        convert(&mut host, false).unwrap_err().code,
        "injected_write_failure"
    );
    host.knowledge(
        "update_knowledge",
        json!({"artifact_id": "report:a", "content": "New legacy edit"}),
    )
    .unwrap();
    assert_eq!(
        convert(&mut host, false).unwrap_err().code,
        "document_conversion_conflict"
    );
    assert_eq!(document(&mut host, "report:a")["content"], "First");
    assert_eq!(
        host.knowledge("get_knowledge", json!({"artifact_id": "report:a"}))
            .unwrap()["artifact"]["content"],
        "New legacy edit"
    );
}

#[test]
fn retry_after_partial_retirement_keeps_the_complete_identity_map_and_document_edits() {
    let mut host = seeded();
    legacy(
        &mut host,
        "report:a",
        "report",
        "[Second](lumvise://artifact/report%3Ab)",
        &[],
    );
    legacy(
        &mut host,
        "report:b",
        "report",
        "[First](lumvise://artifact/report%3Aa)",
        &[],
    );
    host.fail_resource_write = Some(1);
    assert_eq!(
        convert(&mut host, false).unwrap_err().code,
        "injected_write_failure"
    );
    let first = document(&mut host, "report:a");
    host.knowledge("update_document", json!({"document_id": "report:a", "project_root": "/project",
        "expected_revision": first["revision"], "content": "Human change after partial retirement"})).unwrap();
    convert(&mut host, false).unwrap();
    assert_eq!(
        document(&mut host, "report:a")["content"],
        "Human change after partial retirement"
    );
    assert_eq!(
        document(&mut host, "report:b")["content"],
        "[First](lumvise://document/report%3Aa)"
    );
    assert_eq!(convert(&mut host, false).unwrap()["total"], 0);
}

#[test]
fn a_deleted_staged_document_is_not_resurrected_by_a_conversion_retry() {
    let mut host = seeded();
    legacy(&mut host, "report:a", "report", "First", &[]);
    host.fail_resource_write = Some(0);
    assert_eq!(
        convert(&mut host, false).unwrap_err().code,
        "injected_write_failure"
    );
    host.knowledge(
        "delete_document",
        json!({"project_root": "/project", "document_id": "report:a"}),
    )
    .unwrap();
    assert_eq!(
        convert(&mut host, false).unwrap_err().code,
        "document_conversion_conflict"
    );
    assert!(
        host.knowledge("get_knowledge", json!({"artifact_id": "report:a"}))
            .is_ok()
    );
}

#[test]
fn rulesets_and_generated_source_context_keep_their_workflow_outside_the_report_library() {
    let mut host = seeded();
    legacy(
        &mut host,
        "ruleset:a",
        "specification",
        "Agent instructions",
        &["agents", "ruleset"],
    );
    legacy(
        &mut host,
        "functional:a",
        "report",
        "Functional source context",
        &["cultivation-functional"],
    );
    convert(&mut host, false).unwrap();
    assert_eq!(document(&mut host, "ruleset:a")["document_kind"], "ruleset");
    assert_eq!(
        document(&mut host, "functional:a")["document_kind"],
        "context"
    );
    assert_eq!(
        host.knowledge(
            "list_documents",
            json!({"project_root": "/project", "document_kind": "report"})
        )
        .unwrap()["total"],
        0
    );
}

#[test]
fn retained_artifact_dependencies_and_existing_documents_follow_converted_document_identity() {
    let mut host = seeded();
    legacy(&mut host, "report:a", "report", "First", &[]);
    legacy(
        &mut host,
        "spec:a",
        "specification",
        "[Report](lumvise://artifact/report%3Aa)",
        &[],
    );
    host.knowledge("update_knowledge", json!({"artifact_id": "spec:a", "dependencies": [{"target": {"target_kind": "artifact", "artifact_id": "report:a"}}]})).unwrap();
    let mut existing = report();
    existing["references"] = json!([{"target_kind": "artifact", "artifact_id": "report:a"}, {"target_kind": "document", "document_id": "report:a"}]);
    host.knowledge("create_document", existing).unwrap();
    convert(&mut host, false).unwrap();
    let spec = host
        .knowledge("get_knowledge", json!({"artifact_id": "spec:a"}))
        .unwrap();
    assert_eq!(
        spec["artifact"]["content"],
        "[Report](lumvise://document/report%3Aa)"
    );
    assert_eq!(
        spec["artifact"]["dependencies"][0]["target"],
        json!({"target_kind": "semantic_element", "semantic_element_id": "knowledge-document:report:a"})
    );
    assert_eq!(
        document(&mut host, "report:architecture")["references"][0],
        json!({"target_kind": "document", "document_id": "report:a"})
    );
    assert_eq!(
        document(&mut host, "report:architecture")["references"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

fn board_scene() -> Value {
    json!({"nodes": [{"id": "native-shape", "kind": "group", "text": "Keep drawing", "x": 19, "y": 23, "width": 100, "height": 40,
        "nativeElements": [{"id": "image-one", "type": "image", "fileId": "image-1", "customData": {"human": [1, 2, 3]}}]},
        {"id": "report-link", "kind": "artifact", "artifactId": "report:a", "x": 25, "y": 300, "width": 200, "height": 100}],
        "edges": [], "layers": [], "files": {"image-1": {"id": "image-1", "contentRef": "canvas-file:board:a:image-one", "mimeType": "image/png"}}})
}

#[test]
fn board_conversion_keeps_native_scene_and_image_bytes_through_refresh_reopen_and_archive() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("boards.sqlite");
    let mut host = DocumentBoundaryHost::with_persistence(Arc::new(
        LocalPersistence::open(&database).unwrap(),
    ));
    host.semantic("ingest_index_batch", source_batch()).unwrap();
    let scene = board_scene();
    legacy(
        &mut host,
        "board:a",
        "definition",
        &serde_json::to_string(&scene).unwrap(),
        &["canvas"],
    );
    legacy(&mut host, "report:a", "report", "Linked report", &[]);
    host.execute(SemanticOperation::ArtifactBlobPut {
        content_ref: "canvas-file:board:a:image-one".into(),
        artifact_id: "board:a".into(),
        media_type: "image/png".into(),
        content: vec![0, 255, 17, 3],
    });
    convert(&mut host, false).unwrap();
    host.semantic("ingest_index_batch", source_batch()).unwrap();
    drop(host);
    let mut reopened = DocumentBoundaryHost::with_persistence(Arc::new(
        LocalPersistence::open(&database).unwrap(),
    ));
    assert_board(&mut reopened, &scene, "/project");
    let SemanticResult::ProjectArchive(archive) =
        reopened.execute(SemanticOperation::ProjectArchive {
            project_root: "/project".into(),
        })
    else {
        panic!("project archive expected")
    };
    let archive_path = directory.path().join("board.pz");
    archive
        .write(&archive_path, &InvocationControl::sixty_seconds())
        .unwrap();
    let mut imported = DocumentBoundaryHost::new();
    imported.execute(SemanticOperation::ImportPzSnapshot {
        project_root: "/imported".into(),
        input_path: archive_path.to_string_lossy().into(),
    });
    assert_board(&mut imported, &scene, "/imported");
}

fn assert_board(host: &mut DocumentBoundaryHost, original: &Value, root: &str) {
    let board = host
        .knowledge(
            "get_document",
            json!({"document_id": "board:a", "project_root": root}),
        )
        .unwrap()["document"]
        .clone();
    assert_eq!(board["attachment_owner_id"], "board:a");
    assert_eq!(board["document_kind"], "canvas");
    let scene: Value = serde_json::from_str(board["content"].as_str().unwrap()).unwrap();
    assert_eq!(scene["nodes"][0], original["nodes"][0]);
    assert_eq!(scene["files"], original["files"]);
    assert_eq!(scene["nodes"][1]["kind"], "document");
    assert_eq!(scene["nodes"][1]["documentId"], "report:a");
    let SemanticResult::ArtifactBlob(Some(blob)) =
        host.execute(SemanticOperation::ArtifactBlobGet {
            content_ref: "canvas-file:board:a:image-one".into(),
        })
    else {
        panic!("image bytes expected")
    };
    assert_eq!(blob.content, vec![0, 255, 17, 3]);
    let SemanticResult::Artifact(Some(resource)) = host.execute(SemanticOperation::Artifact {
        artifact_id: "board:a".into(),
    }) else {
        panic!("attachment owner expected")
    };
    assert_eq!(
        resource.semantic_element_id,
        board["semantic_element_id"].as_str().unwrap()
    );
    assert_eq!(resource.artifact_kind, "document_assets");
    assert!(resource.content.is_none());
    assert!(resource.metadata.get("knowledge").is_none());
}

#[test]
fn a_new_canvas_document_creates_its_explicit_attachment_owner_in_the_same_publication() {
    let mut host = DocumentBoundaryHost::new();
    let mut request = report();
    request["document_kind"] = json!("canvas");
    request["content"] =
        json!(serde_json::to_string(&json!({"nodes": [], "edges": [], "layers": []})).unwrap());
    let created = host.knowledge("create_document", request).unwrap();
    let owner = created["document"]["attachment_owner_id"].as_str().unwrap();
    let SemanticResult::Artifact(Some(resource)) = host.execute(SemanticOperation::Artifact {
        artifact_id: owner.into(),
    }) else {
        panic!("new attachment owner expected")
    };
    assert_eq!(
        resource.semantic_element_id,
        created["document"]["semantic_element_id"].as_str().unwrap()
    );
    assert!(resource.metadata.get("knowledge").is_none());
}

#[test]
fn product_artifact_creation_cannot_replace_a_document_attachment_owner() {
    let mut host = DocumentBoundaryHost::new();
    let mut request = report();
    request["document_kind"] = json!("canvas");
    let created = host.knowledge("create_document", request).unwrap();
    let owner = created["document"]["attachment_owner_id"].as_str().unwrap();
    let failure = host.knowledge("create_knowledge", json!({"artifact_id": owner,
        "semantic_element_id": created["document"]["semantic_element_id"], "knowledge_type": "specification",
        "title": "Collision", "content": "Must not replace attachment ownership", "project_root": "/project"})).unwrap_err();
    assert_eq!(failure.code, "knowledge_id_conflict");
    let SemanticResult::Artifact(Some(resource)) = host.execute(SemanticOperation::Artifact {
        artifact_id: owner.into(),
    }) else {
        panic!("original attachment resource expected")
    };
    assert_eq!(resource.artifact_kind, "document_assets");
    assert_eq!(host.knowledge("get_document", identity()).unwrap(), created);
}

#[test]
fn retired_knowledge_with_an_inactive_source_owner_remains_convertible() {
    let mut host = seeded();
    legacy(
        &mut host,
        "report:a",
        "report",
        "Retain historical content",
        &[],
    );
    let mut removed = source_batch();
    removed["semantic_elements"] = json!([]);
    host.semantic("ingest_index_batch", removed).unwrap();
    assert_eq!(convert(&mut host, false).unwrap()["total"], 1);
    assert_eq!(
        document(&mut host, "report:a")["content"],
        "Retain historical content"
    );
}
