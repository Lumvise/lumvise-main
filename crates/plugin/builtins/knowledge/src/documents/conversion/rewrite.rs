//! Rewrites only typed links and Lumvise URLs. Native drawing properties stay intact.

use std::collections::BTreeSet;

use lumvise_contracts::KnowledgeDocument;
use lumvise_plugin_sdk::PluginError;
use serde_json::{Value, json};

use super::super::records;

pub(super) fn document(
    document: &mut KnowledgeDocument,
    converted: &BTreeSet<String>,
) -> Result<(), PluginError> {
    document.content = content_text(&document.content, converted);
    let mut references =
        serde_json::to_value(&document.references).expect("serializable references");
    value(&mut references, converted);
    document.references = crate::parse(references, "converted document references")?;
    let mut seen = std::collections::HashSet::new();
    document
        .references
        .retain(|reference| seen.insert(reference.clone()));
    let provenance = document
        .metadata
        .as_object_mut()
        .and_then(|fields| fields.remove("legacy_knowledge"));
    value(&mut document.metadata, converted);
    if let Some(provenance) = provenance {
        document.metadata["legacy_knowledge"] = provenance;
    }
    Ok(())
}

pub(super) fn value(value: &mut Value, converted: &BTreeSet<String>) {
    match value {
        Value::String(text) => *text = links(text, converted),
        Value::Array(values) => values
            .iter_mut()
            .for_each(|item| self::value(item, converted)),
        Value::Object(fields) => {
            typed_target(fields, converted);
            for item in fields.values_mut() {
                self::value(item, converted);
            }
        }
        _ => {}
    }
}

fn typed_target(fields: &mut serde_json::Map<String, Value>, converted: &BTreeSet<String>) {
    for (tag, old_id, new_id) in [
        ("target_kind", "artifact_id", "document_id"),
        ("kind", "artifactId", "documentId"),
        ("kind", "id", "id"),
    ] {
        if fields.get(tag).and_then(Value::as_str) != Some("artifact") {
            continue;
        }
        if fields
            .get(old_id)
            .and_then(Value::as_str)
            .is_some_and(|id| converted.contains(id))
        {
            let id = fields.remove(old_id).expect("checked target identity");
            fields.insert(tag.into(), json!("document"));
            fields.insert(new_id.into(), id);
        }
    }
}

pub(super) fn artifact(artifact: &mut Value, converted: &BTreeSet<String>) {
    if let Some(dependencies) = artifact["dependencies"].as_array_mut() {
        for dependency in dependencies.iter_mut() {
            let target = &dependency["target"];
            if let Some(id) = target["artifact_id"]
                .as_str()
                .filter(|id| converted.contains(*id))
            {
                dependency["target"] = json!({"target_kind": "semantic_element",
                    "semantic_element_id": records::element_id(id)});
            }
        }
        let mut seen = Vec::new();
        dependencies.retain(|dependency| {
            if seen.contains(dependency) {
                return false;
            }
            seen.push(dependency.clone());
            true
        });
    }
    if let Some(content) = artifact["content"].as_str() {
        artifact["content"] = json!(content_text(content, converted));
    }
    value(&mut artifact["metadata"], converted);
}

pub(super) fn content_text(content: &str, converted: &BTreeSet<String>) -> String {
    let Ok(mut document) = serde_json::from_str::<Value>(content) else {
        return links(content, converted);
    };
    let original = document.clone();
    value(&mut document, converted);
    if document == original {
        return content.into();
    }
    serde_json::to_string(&document).expect("JSON Value remains serializable")
}

fn links(text: &str, converted: &BTreeSet<String>) -> String {
    const PREFIX: &str = "lumvise://artifact/";
    let mut result = String::new();
    let mut remaining = text;
    while let Some(start) = remaining.find(PREFIX) {
        result.push_str(&remaining[..start]);
        remaining = &remaining[start + PREFIX.len()..];
        let end = remaining.find(link_boundary).unwrap_or(remaining.len());
        let id = &remaining[..end];
        let is_document = converted
            .iter()
            .any(|candidate| candidate == id || encode(candidate) == id);
        result.push_str(if is_document {
            "lumvise://document/"
        } else {
            PREFIX
        });
        result.push_str(id);
        remaining = &remaining[end..];
    }
    result.push_str(remaining);
    result
}

fn link_boundary(character: char) -> bool {
    character.is_whitespace() || matches!(character, ')' | ']' | '>' | '<' | '"' | '\'' | '?' | '#')
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'!'
            | b'\''
            | b'('
            | b')'
            | b'*' => (byte as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}
