use lumvise_db_core::{DbCore, SemanticArtifact, SemanticElement};
use serde_json::json;

#[test]
fn file_level_modalities_resolve_annotations_and_segments() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .sync_semantic_structure(
            "/repo",
            &[
                folder_element("assets", "assets"),
                media_file("image-file", "assets/diagram.png", "image", "assets"),
                media_file("audio-file", "assets/call.wav", "audio", "assets"),
                media_file("video-file", "assets/demo.mp4", "video", "assets"),
                media_segment("image-region", "image_region", "image-file"),
                media_segment("audio-segment", "audio_segment", "audio-file"),
                media_segment("video-segment", "video_segment", "video-file"),
            ],
            &[],
        )
        .unwrap();

    annotate_path(
        &storage,
        "assets/diagram.png",
        "image:10,10,30,30",
        "image-note",
    );
    annotate_path(
        &storage,
        "assets/call.wav",
        "audio:12.0..18.0",
        "audio-note",
    );
    annotate_path(
        &storage,
        "assets/demo.mp4",
        "video:00:01..00:05",
        "video-note",
    );
    storage
        .upsert_artifact(&artifact("image-region-note", "image-region"))
        .unwrap();
    storage
        .upsert_artifact(&artifact("audio-segment-note", "audio-segment"))
        .unwrap();
    storage
        .upsert_artifact(&artifact("video-segment-note", "video-segment"))
        .unwrap();

    assert_file_annotation(&storage, "image-note", "image-file", "image");
    assert_file_annotation(&storage, "audio-note", "audio-file", "audio");
    assert_file_annotation(&storage, "video-note", "video-file", "video");
    assert_parent(&storage, "image-region", "image-file");
    assert_parent(&storage, "audio-segment", "audio-file");
    assert_parent(&storage, "video-segment", "video-file");
}

#[test]
fn file_level_image_and_audio_copies_inherit_annotations() {
    for (media_kind, extension, selector) in [
        ("image", "png", "image:crop"),
        ("audio", "wav", "audio:12.0..18.0"),
    ] {
        assert_media_copy_inherits(media_kind, extension, selector);
    }
}

fn assert_media_copy_inherits(media_kind: &str, extension: &str, selector: &str) {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let original_path = format!("assets/original.{extension}");
    let copy_path = format!("assets/copy.{extension}");
    storage
        .sync_semantic_structure(
            "/repo",
            &[
                folder_element("assets", "assets"),
                media_file_with_fingerprint(
                    "media-original",
                    &original_path,
                    media_kind,
                    "assets",
                    "fp1:0000000000000001:original",
                ),
            ],
            &[],
        )
        .unwrap();
    annotate_path(&storage, &original_path, selector, "media-note");
    storage
        .sync_semantic_structure(
            "/repo",
            &[
                folder_element("assets", "assets"),
                media_file_with_fingerprint(
                    "media-original",
                    &original_path,
                    media_kind,
                    "assets",
                    "fp1:0000000000000001:original",
                ),
                media_file_with_fingerprint(
                    "media-copy",
                    &copy_path,
                    media_kind,
                    "assets",
                    "fp1:0000000000000003:copy",
                ),
            ],
            &[],
        )
        .unwrap();

    let inherited = storage
        .artifacts_for_element_with_inheritance("media-copy")
        .unwrap();

    assert_eq!(inherited.len(), 1);
    assert_eq!(inherited[0].semantic_element_id, "media-copy");
    assert_eq!(inherited[0].metadata["association_kind"], "inherited");
    assert_eq!(inherited[0].metadata["association_precaution"], true);
    assert_eq!(
        inherited[0].metadata["inherited_from_semantic_element_id"],
        "media-original"
    );
}

#[test]
fn file_level_media_copy_does_not_cross_modalities() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .sync_semantic_structure(
            "/repo",
            &[
                folder_element("assets", "assets"),
                media_file_with_fingerprint(
                    "image-original",
                    "assets/original.png",
                    "image",
                    "assets",
                    "fp1:0000000000000001:original",
                ),
                media_file_with_fingerprint(
                    "audio-copy",
                    "assets/copy.wav",
                    "audio",
                    "assets",
                    "fp1:0000000000000003:copy",
                ),
            ],
            &[],
        )
        .unwrap();
    annotate_path(&storage, "assets/original.png", "image:crop", "crop-note");

    let inherited = storage
        .artifacts_for_element_with_inheritance("audio-copy")
        .unwrap();

    assert!(inherited.is_empty());
}

fn annotate_path(
    storage: &lumvise_db_core::SemanticStorage<'_>,
    path: &str,
    selector: &str,
    artifact_id: &str,
) {
    storage
        .upsert_media_annotation_for_path("/repo", path, selector, &artifact(artifact_id, ""))
        .unwrap();
}

fn assert_file_annotation(
    storage: &lumvise_db_core::SemanticStorage<'_>,
    artifact_id: &str,
    element_id: &str,
    media_kind: &str,
) {
    let stored = storage.artifact(artifact_id).unwrap().unwrap();
    assert_eq!(stored.semantic_element_id, element_id);
    assert_eq!(stored.metadata["semantic_element_type"], "file");
    assert_eq!(stored.metadata["media_kind"], media_kind);
    assert_eq!(stored.metadata["parent_element_id"], "assets");
}

fn assert_parent(storage: &lumvise_db_core::SemanticStorage<'_>, child_id: &str, parent_id: &str) {
    let child = storage.element(child_id).unwrap().unwrap();
    assert_eq!(child.parent_element_id.as_deref(), Some(parent_id));
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

fn media_file(id: &str, path: &str, media_kind: &str, parent_id: &str) -> SemanticElement {
    media_file_with_fingerprint(
        id,
        path,
        media_kind,
        parent_id,
        &format!("fp1:0000000000000001:{id}"),
    )
}

fn media_file_with_fingerprint(
    id: &str,
    path: &str,
    media_kind: &str,
    parent_id: &str,
    fingerprint: &str,
) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".to_string(),
        semantic_element_id: id.to_string(),
        semantic_source_id: "source".to_string(),
        path: path.to_string(),
        element_kind: "file".to_string(),
        name: path.rsplit('/').next().unwrap_or(path).to_string(),
        parent_element_id: Some(parent_id.to_string()),
        content_fingerprint: Some(fingerprint.to_string()),
        start_line: None,
        end_line: None,
        lifecycle: "active".to_string(),
        match_evidence: None,
        metadata: json!({
            "media_kind": media_kind,
            "mime_type": mime_type(media_kind),
        }),
    }
}

fn media_segment(id: &str, kind: &str, parent_id: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".to_string(),
        semantic_element_id: id.to_string(),
        semantic_source_id: "source".to_string(),
        path: "assets/media".to_string(),
        element_kind: kind.to_string(),
        name: id.to_string(),
        parent_element_id: Some(parent_id.to_string()),
        content_fingerprint: Some(format!("fp1:0000000000000002:{id}")),
        start_line: None,
        end_line: None,
        lifecycle: "active".to_string(),
        match_evidence: None,
        metadata: json!({
            "fingerprint_algorithm": format!("{kind}-fp1-v1"),
            "anchor_selector": { "kind": kind }
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

fn mime_type(media_kind: &str) -> &'static str {
    match media_kind {
        "image" => "image/png",
        "audio" => "audio/wav",
        "video" => "video/mp4",
        _ => "application/octet-stream",
    }
}
