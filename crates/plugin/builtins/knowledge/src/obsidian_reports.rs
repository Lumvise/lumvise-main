//! Projects report artifacts into read-only Obsidian pages.
//!
//! Report derivation remains owned by the C4/Nucleus domain. Knowledge calls this module only
//! while assembling the single revision-consistent Obsidian projection.

use serde_json::{Value, json};

use crate::KnowledgeArtifact;
use crate::projection_identity::canonical_projection_json;

pub(crate) fn is_report(artifact: &KnowledgeArtifact) -> bool {
    artifact.metadata["cultivation_report"].as_bool() == Some(true)
        || artifact.tags.iter().any(|tag| {
            matches!(
                tag.as_str(),
                "nucleus" | "cultivation-report" | "cultivation-nucleus"
            )
        })
}

pub(crate) fn project(project_root: &str, artifact: &KnowledgeArtifact) -> Value {
    let properties = c4_report_properties(artifact);
    let content_hash = report_content_hash(artifact, &properties);
    let target_path = artifact.metadata["nucleus"]["target_path"]
        .as_str()
        .unwrap_or_default();
    let path_hint = artifact.metadata["obsidian"]["path"]
        .as_str()
        .unwrap_or("C4 Architecture.md");
    let title = report_projection_title(path_hint, artifact);
    json!({
        "elementId": artifact.artifact_id, "space": "nuclei", "kind": "nucleus_report",
        "title": title, "markdown": artifact.content, "contentMd5": content_hash,
        "pathHint": path_hint, "sourceId": project_root,
        "sourceRefs": [{"entityKind": "semantic_element",
            "entityId": artifact.semantic_element_id, "path": target_path}],
        "syncToken": format!("artifact:{}:{content_hash}", artifact.artifact_id),
        "changeMarker": format!("artifact:{}:{content_hash}", artifact.artifact_id),
        "ownership": {"content": "lumvise", "source": "lumvise"},
        "writePolicy": "readonly", "properties": properties,
        "children": [], "artifacts": [], "linkTargets": []
    })
}

fn report_projection_title(path_hint: &str, artifact: &KnowledgeArtifact) -> String {
    let report_name = std::path::Path::new(path_hint)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or(&artifact.title);
    format!(
        "{} ({})",
        report_name,
        crate::artifact::knowledge_type_label(&artifact.knowledge_type)
    )
}

fn report_content_hash(artifact: &KnowledgeArtifact, properties: &Value) -> String {
    let signature = if properties.as_object().is_none_or(serde_json::Map::is_empty) {
        artifact.content.clone()
    } else {
        format!(
            "{}\n\nproperties:{}",
            artifact.content,
            canonical_projection_json(properties)
        )
    };
    crate::functional::stable_content_hash(&signature)
}

fn c4_report_properties(artifact: &KnowledgeArtifact) -> Value {
    let project_root = artifact.metadata["nucleus"]["project_root"]
        .as_str()
        .unwrap_or_default();
    let target_id = artifact.metadata["nucleus"]["target_element_id"]
        .as_str()
        .unwrap_or(&artifact.semantic_element_id);
    json!({"c4Update": {
        "label": "Update C4 Architecture",
        "href": format!("obsidian://lumvise-c4?sourceId={}&elementId={}&refresh=true",
            url_encode(project_root), url_encode(target_id)),
        "method": "POST", "endpoint": "/api/knowledge/c4-nucleus",
        "payload": {"project_root": project_root, "target_id": target_id, "refresh": true}
    }})
}

fn url_encode(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}
