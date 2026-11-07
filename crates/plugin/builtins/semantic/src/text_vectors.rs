//! Embeds the visible text of `html_section` elements.
//!
//! The page file is read through `project.source`, the project indexer extracts each
//! section's words with the fingerprint it indexed, and the words are embedded and dropped.
//! Only the vector and fingerprint reach storage: the text is never persisted.

use std::collections::{BTreeMap, HashMap};

use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

use crate::models::SemanticElement;
use crate::query::embed_texts;
use crate::storage;

/// `project.source` reads a line range; this end line reads to the end of any real page.
const WHOLE_FILE_END_LINE: u64 = u32::MAX as u64;

/// Stores text vectors for the active `html_section` elements in `elements`.
///
/// Each page is read once. A section gets a vector only when its indexed fingerprint
/// matches a section the indexer finds in the current page bytes; sections without
/// words get none.
pub(crate) fn index_html_sections(
    context: &mut PluginContext<'_>,
    project_root: &str,
    elements: &[SemanticElement],
) -> Result<usize, PluginError> {
    let mut by_path: BTreeMap<&str, Vec<&SemanticElement>> = BTreeMap::new();
    for element in elements
        .iter()
        .filter(|element| element.element_kind == "html_section" && element.lifecycle == "active")
    {
        by_path
            .entry(element.path.as_str())
            .or_default()
            .push(element);
    }
    let mut sections: Vec<(String, String, String)> = Vec::new();
    for (path, page_elements) in by_path {
        let Some(page) = read_page(context, project_root, path) else {
            // shortcut: an unreadable or unparsable page keeps its sections without text vectors until the next rebuild, because name vectors still serve them.
            continue;
        };
        let texts: HashMap<String, String> = page
            .into_iter()
            .map(|section| (section.content_fingerprint, section.text))
            .collect();
        for element in page_elements {
            let Some(fingerprint) = element.content_fingerprint.as_deref() else {
                continue;
            };
            match texts.get(fingerprint).map(|text| text.trim()) {
                Some(text) if !text.is_empty() => sections.push((
                    element.semantic_element_id.clone(),
                    fingerprint.to_owned(),
                    text.to_owned(),
                )),
                _ => {}
            }
        }
    }
    if sections.is_empty() {
        return Ok(0);
    }
    let texts = sections
        .iter()
        .map(|(_, _, text)| text.as_str())
        .collect::<Vec<_>>();
    let embedded = embed_texts(context, &texts).ok_or_else(|| {
        PluginError::new("semantic_embedding_failed", "embedding batch failed", true)
    })?;
    let records: Vec<Value> = sections
        .into_iter()
        .zip(embedded.vectors)
        .map(|((id, fingerprint, _), vector)| {
            json!({"semantic_element_id": id, "project_root": project_root,
                "content_fingerprint": fingerprint,
                "vector": {"engine_id": embedded.engine_id, "model": embedded.model,
                    "dimensions": vector.len(), "vector": vector, "normalized": false,
                    "metadata": {}}})
        })
        .collect();
    storage::store_element_text_vectors(context, project_root, records)
}

fn read_page(
    context: &mut PluginContext<'_>,
    project_root: &str,
    path: &str,
) -> Option<Vec<lumvise_project_indexer::HtmlSectionText>> {
    let output = context
        .host_call(
            "project.source",
            json!({"operation": "read", "project_root": project_root, "path": path,
                "start_line": 1, "end_line": WHOLE_FILE_END_LINE}),
        )
        .ok()?;
    let text = output["text"].as_str()?;
    lumvise_project_indexer::html_section_texts(text).ok()
}
