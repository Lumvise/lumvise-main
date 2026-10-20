//! Projects independent report documents into read-only Obsidian pages.
//!
//! Report derivation remains owned by the C4/Nucleus domain. Knowledge calls this module only
//! while assembling the single revision-consistent Obsidian projection.

use serde_json::{Value, json};

use crate::projection_identity::canonical_projection_json;
use lumvise_contracts::{KnowledgeDocument, KnowledgeDocumentKind, KnowledgeDocumentReference};

#[cfg(test)]
mod tests;

pub(crate) fn is_report(document: &KnowledgeDocument) -> bool {
    document.document_kind == KnowledgeDocumentKind::Report
        && crate::lifecycle::is_active(&document.lifecycle, chrono::Utc::now())
}

pub(crate) fn project(document: &KnowledgeDocument) -> Value {
    let properties = c4_report_properties(document);
    let content_hash = report_content_hash(document, &properties);
    let path_hint = report_path(document);
    let title = format!("{} (Report)", document.title);
    json!({
        "elementId": document.document_id, "space": "reports", "kind": "report",
        "title": title, "markdown": document.content, "contentMd5": content_hash,
        "pathHint": path_hint, "sourceId": document.project_root,
        "sourceRefs": report_references(document),
        "syncToken": format!("document:{}:{content_hash}", document.document_id),
        "changeMarker": format!("document:{}:{content_hash}", document.document_id),
        "ownership": {"content": "lumvise", "source": "lumvise"},
        "writePolicy": "readonly", "properties": properties,
        "children": [], "artifacts": [], "linkTargets": []
    })
}

fn report_path(document: &KnowledgeDocument) -> String {
    if document.path.is_none()
        && let Some(path) = document.metadata["obsidian"]["path"].as_str()
    {
        return path.into();
    }
    let title = document.title.replace(['/', '\\'], "-");
    let filename = format!("{title}.md");
    document
        .path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| format!("{}/{filename}", path.trim_matches('/')))
        .unwrap_or(filename)
}

fn report_references(document: &KnowledgeDocument) -> Vec<Value> {
    std::iter::once(json!({"entityKind":"document","entityId":document.document_id,"path":document.path.as_deref().unwrap_or_default()}))
        .chain(document.references.iter().map(|reference| match reference {
            KnowledgeDocumentReference::SemanticElement {semantic_element_id} => json!({"entityKind":"semantic_element","entityId":semantic_element_id,"path":""}),
            KnowledgeDocumentReference::Artifact {artifact_id} => json!({"entityKind":"artifact","entityId":artifact_id,"path":""}),
            KnowledgeDocumentReference::Document {document_id} => json!({"entityKind":"document","entityId":document_id,"path":""}),
        })).collect()
}

fn report_content_hash(document: &KnowledgeDocument, properties: &Value) -> String {
    let signature = if properties.as_object().is_none_or(serde_json::Map::is_empty) {
        document.content.clone()
    } else {
        format!(
            "{}\n\nproperties:{}",
            document.content,
            canonical_projection_json(properties)
        )
    };
    crate::functional::stable_content_hash(&signature)
}

fn c4_report_properties(document: &KnowledgeDocument) -> Value {
    let Some(target_id) = document.metadata["nucleus"]["target_element_id"]
        .as_str()
        .filter(|_| document.tags.iter().any(|tag| tag == "scoped-c4"))
    else {
        return json!({});
    };
    let project_root = document.project_root.as_str();
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
