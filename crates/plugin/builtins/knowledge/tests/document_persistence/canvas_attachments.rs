//! Canvas attachments use current document ownership through the real persistence boundary.
use super::*;

fn native_scene(content_ref: &str) -> Value {
    json!({"nodes":[{"id":"native-shape","kind":"group","text":"Keep drawing","x":19,"y":23,"width":100,"height":40,
        "nativeElements":[{"id":"image-one","type":"image","fileId":"image-1","customData":{"human":[1,2,3]}}]},
        {"id":"report-link","kind":"document","documentId":"report:a","x":25,"y":300,"width":200,"height":100}],
        "edges":[],"layers":[],"files":{"image-1":{"id":"image-1","contentRef":content_ref,"mimeType":"image/png"}}})
}

fn saved_board(host: &mut DocumentBoundaryHost) -> (Value, Value, String) {
    host.knowledge(
        "create_document",
        json!({"project_root":"/project","document_id":"report:a",
        "document_kind":"report","title":"Linked report","content":"Linked report"}),
    )
    .unwrap();
    let created=host.knowledge("create_document",json!({"project_root":"/project","document_id":"board:a",
        "document_kind":"canvas","title":"Native canvas","content":"{\"nodes\":[],\"edges\":[],\"layers\":[]}"})).unwrap()["document"].clone();
    let owner = created["attachment_owner_id"].as_str().unwrap();
    let content_ref = format!("canvas-file:{owner}:image-one");
    host.execute(SemanticOperation::ArtifactBlobPut {
        content_ref: content_ref.clone(),
        artifact_id: owner.into(),
        media_type: "image/png".into(),
        content: vec![0, 255, 17, 3],
    });
    let scene = native_scene(&content_ref);
    let saved=host.knowledge("update_document",json!({"project_root":"/project","document_id":"board:a",
        "expected_revision":created["revision"],"expected_semantic_element_id":created["semantic_element_id"],
        "content":serde_json::to_string(&scene).unwrap()})).unwrap()["document"].clone();
    (saved, scene, content_ref)
}

fn assert_native_board(
    host: &mut DocumentBoundaryHost,
    root: &str,
    scene: &Value,
    content_ref: &str,
) {
    let board = host
        .knowledge(
            "get_document",
            json!({"project_root":root,"document_id":"board:a"}),
        )
        .unwrap()["document"]
        .clone();
    assert_eq!(board["document_kind"], "canvas");
    assert_eq!(
        serde_json::from_str::<Value>(board["content"].as_str().unwrap()).unwrap(),
        *scene
    );
    let SemanticResult::ArtifactBlob(Some(blob)) =
        host.execute(SemanticOperation::ArtifactBlobGet {
            content_ref: content_ref.into(),
        })
    else {
        panic!("expected exact image bytes")
    };
    assert_eq!(blob.content, vec![0, 255, 17, 3]);
    let SemanticResult::Artifact(Some(resource)) = host.execute(SemanticOperation::Artifact {
        artifact_id: board["attachment_owner_id"].as_str().unwrap().into(),
    }) else {
        panic!("expected current attachment owner")
    };
    assert_eq!(resource.artifact_kind, "document_assets");
    assert!(resource.metadata.get("knowledge").is_none());
}

fn archive_board(host: &DocumentBoundaryHost, path: &std::path::Path) {
    let SemanticResult::ProjectArchive(archive) = host.execute(SemanticOperation::ProjectArchive {
        project_root: "/project".into(),
    }) else {
        panic!("expected project archive")
    };
    archive
        .write(path, &InvocationControl::sixty_seconds())
        .unwrap();
}

#[test]
fn native_canvas_and_image_bytes_survive_source_refresh_reopen_and_archive_relocation() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("boards.sqlite");
    let mut host = DocumentBoundaryHost::with_persistence(Arc::new(
        LocalPersistence::open(&database).unwrap(),
    ));
    source_publication::publish(&host, source_batch());
    let (_, scene, content_ref) = saved_board(&mut host);
    let mut refreshed = source_batch();
    refreshed["semantic_elements"][0]["content_fingerprint"] =
        json!("fp1:0000000000000002:refreshed");
    source_publication::publish(&host, refreshed);
    drop(host);
    let mut reopened = DocumentBoundaryHost::with_persistence(Arc::new(
        LocalPersistence::open(&database).unwrap(),
    ));
    assert_native_board(&mut reopened, "/project", &scene, &content_ref);
    let archive = directory.path().join("board.pz");
    archive_board(&reopened, &archive);
    let mut imported = DocumentBoundaryHost::new();
    imported.execute(SemanticOperation::ImportPzSnapshot {
        project_root: "/imported".into(),
        input_path: archive.to_string_lossy().into(),
    });
    assert_native_board(&mut imported, "/imported", &scene, &content_ref);
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
