use lumvise_db_core::{DbCore, SemanticArtifact, SemanticElement};
use serde_json::json;

#[test]
fn anchored_annotation_resolves_active_media_element_by_path() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .sync_semantic_structure(
            "/repo",
            &[
                media_element("image-a", "assets/diagram.png", "image"),
                media_element("audio-a", "assets/call.wav", "audio"),
                media_element("video-a", "assets/demo.mp4", "video"),
            ],
            &[],
        )
        .unwrap();

    storage
        .upsert_media_annotation_for_path(
            "/repo",
            "assets/diagram.png",
            "image:10,10,30,30",
            &artifact("note", ""),
        )
        .unwrap();

    let stored = storage.artifact("note").unwrap().unwrap();
    assert_eq!(stored.semantic_element_id, "image-a");
    assert_eq!(stored.metadata["anchor_selector"], "image:10,10,30,30");
    assert_eq!(stored.metadata["association_kind"], "direct");
    assert_eq!(stored.metadata["association_confidence"], 100);
    assert_eq!(stored.metadata["semantic_element_id"], "image-a");
    assert_eq!(stored.metadata["semantic_element_type"], "file");
    assert_eq!(stored.metadata["media_kind"], "image");
    assert_eq!(
        stored.metadata["semantic_element_path"],
        "assets/diagram.png"
    );
}

#[test]
fn missing_media_path_rejects_anchored_annotation() {
    let db = DbCore::in_memory().unwrap();
    let error = db
        .storage_manager()
        .semantic_storage()
        .upsert_media_annotation_for_path(
            "/repo",
            "assets/missing.png",
            "image:10,10,30,30",
            &artifact("note", ""),
        )
        .unwrap_err();

    assert!(error.to_string().contains("assets/missing.png"));
    assert!(error.to_string().contains("active media semantic element"));
}

#[test]
fn image_region_annotation_links_to_precise_segment_element() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .sync_semantic_structure(
            "/repo",
            &[
                media_element("image-a", "assets/diagram.png", "image"),
                image_region(
                    "image-a-region-header",
                    "image-a",
                    "image-region-byte-fp1-v1",
                ),
            ],
            &[],
        )
        .unwrap();

    storage
        .upsert_artifact(&artifact("region-note", "image-a-region-header"))
        .unwrap();

    let stored = storage.artifact("region-note").unwrap().unwrap();
    let region = storage.element("image-a-region-header").unwrap().unwrap();
    assert_eq!(stored.semantic_element_id, "image-a-region-header");
    assert_eq!(region.element_kind, "image_region");
    assert_eq!(
        region.metadata["fingerprint_algorithm"],
        "image-region-byte-fp1-v1"
    );
}

#[test]
fn media_inside_folder_keeps_folder_as_parent() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .sync_semantic_structure(
            "/repo",
            &[
                folder_element("assets", "assets"),
                media_file_element("diagram-file", "assets/diagram.png", "image", "assets"),
            ],
            &[],
        )
        .unwrap();

    storage
        .upsert_media_annotation_for_path(
            "/repo",
            "assets/diagram.png",
            "image:10,10,30,30",
            &artifact("folder-note", ""),
        )
        .unwrap();

    let image = storage.element("diagram-file").unwrap().unwrap();
    let stored = storage.artifact("folder-note").unwrap().unwrap();
    assert_eq!(image.element_kind, "file");
    assert_eq!(image.parent_element_id.as_deref(), Some("assets"));
    assert_eq!(stored.metadata["parent_element_id"], "assets");
    assert_eq!(stored.semantic_element_id, "diagram-file");
    assert_eq!(stored.metadata["semantic_element_type"], "file");
    assert_eq!(stored.metadata["media_kind"], "image");
}

#[test]
fn fingerprint_algorithm_mismatch_prevents_segment_identity_reuse() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .sync_semantic_structure(
            "/repo",
            &[image_region(
                "old-region",
                "image-a",
                "image-region-byte-fp1-v1",
            )],
            &[],
        )
        .unwrap();
    storage.sync_semantic_structure("/repo", &[], &[]).unwrap();

    let report = storage
        .sync_semantic_structure(
            "/repo",
            &[image_region(
                "new-region",
                "image-a",
                "image-region-dhash-v1",
            )],
            &[],
        )
        .unwrap();

    assert_eq!(report.identities_reused, 0);
    assert!(storage.element("old-region").unwrap().is_none());
    assert_eq!(
        storage.element("new-region").unwrap().unwrap().lifecycle,
        "active"
    );
}

#[test]
fn ambiguous_similar_media_sources_do_not_inherit_annotations() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .sync_semantic_structure(
            "/repo",
            &[
                media_file_with_fingerprint("image-a", "assets/a.png", "fp1:0000000000000001:a"),
                media_file_with_fingerprint("image-b", "assets/b.png", "fp1:0000000000000003:b"),
                media_file_with_fingerprint(
                    "image-copy",
                    "assets/copy.png",
                    "fp1:0000000000000002:c",
                ),
            ],
            &[],
        )
        .unwrap();
    storage
        .upsert_media_annotation_for_path("/repo", "assets/a.png", "image:a", &artifact("a", ""))
        .unwrap();
    storage
        .upsert_media_annotation_for_path("/repo", "assets/b.png", "image:b", &artifact("b", ""))
        .unwrap();

    let inherited = storage
        .artifacts_for_element_with_inheritance("image-copy")
        .unwrap();

    assert!(inherited.is_empty());
}

fn media_element(id: &str, path: &str, kind: &str) -> SemanticElement {
    let mut element =
        media_element_with_fingerprint(id, path, "file", &format!("fp1:0000000000000001:{id}"));
    element.metadata = json!({
        "media_kind": kind,
        "mime_type": format!("{kind}/unknown"),
    });
    element
}

fn media_file_with_fingerprint(id: &str, path: &str, fingerprint: &str) -> SemanticElement {
    let mut element = media_element_with_fingerprint(id, path, "file", fingerprint);
    element.metadata = json!({
        "media_kind": "image",
        "mime_type": "image/png",
    });
    element
}

fn folder_element(id: &str, path: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".to_string(),
        semantic_element_id: id.to_string(),
        semantic_source_id: "source".to_string(),
        path: path.to_string(),
        element_kind: "folder".to_string(),
        name: path.rsplit('/').next().unwrap_or(path).to_string(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".to_string(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn media_file_element(id: &str, path: &str, media_kind: &str, parent_id: &str) -> SemanticElement {
    let mut element =
        media_element_with_fingerprint(id, path, "file", &format!("fp1:0000000000000001:{id}"));
    element.parent_element_id = Some(parent_id.to_string());
    element.metadata = json!({
        "media_kind": media_kind,
        "mime_type": format!("{media_kind}/png"),
    });
    element
}

fn media_element_with_fingerprint(
    id: &str,
    path: &str,
    kind: &str,
    fingerprint: &str,
) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".to_string(),
        semantic_element_id: id.to_string(),
        semantic_source_id: "source".to_string(),
        path: path.to_string(),
        element_kind: kind.to_string(),
        name: path.rsplit('/').next().unwrap_or(path).to_string(),
        parent_element_id: None,
        content_fingerprint: Some(fingerprint.to_string()),
        start_line: None,
        end_line: None,
        lifecycle: "active".to_string(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn image_region(id: &str, parent_id: &str, algorithm: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".to_string(),
        semantic_element_id: id.to_string(),
        semantic_source_id: "source".to_string(),
        path: "assets/diagram.png".to_string(),
        element_kind: "image_region".to_string(),
        name: "header callout".to_string(),
        parent_element_id: Some(parent_id.to_string()),
        content_fingerprint: Some("fp1:0000000000000002:region-header".to_string()),
        start_line: None,
        end_line: None,
        lifecycle: "active".to_string(),
        match_evidence: None,
        metadata: json!({
            "fingerprint_algorithm": algorithm,
            "anchor_selector": {
                "kind": "image_region",
                "x": 0.25,
                "y": 0.15,
                "window_percent": 0.2
            }
        }),
    }
}

fn artifact(id: &str, element_id: &str) -> SemanticArtifact {
    SemanticArtifact {
        artifact_id: id.to_string(),
        semantic_element_id: element_id.to_string(),
        artifact_kind: "context".to_string(),
        title: id.to_string(),
        content_ref: None,
        content: Some(format!("annotation {id}")),
        searchable_text: Some(format!("annotation {id}")),
        content_size_bytes: Some(format!("annotation {id}").len()),
        metadata: json!({}),
        dependencies: vec![],
    }
}
