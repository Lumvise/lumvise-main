//! Embeds the visible text of `html_section` elements.
//!
//! The page file is read through `project.source`, the project indexer extracts each
//! section's words with the fingerprint it indexed, and the words are embedded and dropped.
//! Only the vector and fingerprint reach storage: the text is never persisted.
//!
//! Pages are processed one at a time, so one unreadable page or rejected section is
//! reported in the returned report and never fails the caller.

use std::collections::{BTreeMap, HashMap};

use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::Serialize;
use serde_json::{Value, json};

use crate::models::SemanticElement;
use crate::query::embed_texts;
use crate::storage;

/// `project.source` reads a line range; this end line reads to the end of any real page.
const WHOLE_FILE_END_LINE: u64 = u32::MAX as u64;

/// What the text-vector pass stored and what it could not, with the causes.
#[derive(Debug, Default, Serialize)]
pub(crate) struct TextVectorReport {
    /// Section vectors stored.
    pub stored: usize,
    /// Sections left without a vector because their page or their record failed.
    pub skipped_sections: usize,
    /// Pages whose read, parse, or embedding failed as a whole.
    pub skipped_pages: usize,
    /// One entry per failed page (`semantic_element_id` is null) or rejected section.
    pub failures: Vec<SkippedText>,
}

/// One page or section that did not get a text vector, and why.
#[derive(Debug, Serialize)]
pub(crate) struct SkippedText {
    pub path: String,
    pub semantic_element_id: Option<String>,
    pub error: String,
}

/// Stores text vectors for the active `html_section` elements in `elements`.
///
/// Each page is read once. A section gets a vector only when its indexed fingerprint
/// matches a section the indexer finds in the current page bytes; sections without
/// words get none.
pub(crate) fn index_html_sections(
    context: &mut PluginContext<'_>,
    project_root: &str,
    elements: &[SemanticElement],
) -> TextVectorReport {
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
    let mut report = TextVectorReport::default();
    for (path, page_elements) in by_path {
        if let Err(error) = index_page(context, project_root, path, &page_elements, &mut report) {
            report.skipped_pages += 1;
            report.skipped_sections += page_elements.len();
            report.failures.push(SkippedText {
                path: path.to_owned(),
                semantic_element_id: None,
                error: error.message,
            });
        }
    }
    report
}

/// Embeds and stores one page. An error means nothing from this page was stored.
fn index_page(
    context: &mut PluginContext<'_>,
    project_root: &str,
    path: &str,
    page_elements: &[&SemanticElement],
    report: &mut TextVectorReport,
) -> Result<(), PluginError> {
    let page_texts: HashMap<String, String> = read_page(context, project_root, path)?
        .into_iter()
        .map(|section| (section.content_fingerprint, section.text))
        .collect();
    let mut sections: Vec<(String, String, String)> = Vec::new();
    for element in page_elements {
        let Some(fingerprint) = element.content_fingerprint.as_deref() else {
            continue;
        };
        match page_texts.get(fingerprint).map(|text| text.trim()) {
            Some(text) if !text.is_empty() => sections.push((
                element.semantic_element_id.clone(),
                fingerprint.to_owned(),
                text.to_owned(),
            )),
            _ => {}
        }
    }
    if sections.is_empty() {
        return Ok(());
    }
    let section_texts = sections
        .iter()
        .map(|(_, _, text)| text.as_str())
        .collect::<Vec<_>>();
    let embedded = embed_texts(context, &section_texts)?;
    let records: Vec<(String, Value)> = sections
        .into_iter()
        .zip(embedded.vectors)
        .map(|((id, fingerprint, _), vector)| {
            let record = json!({"semantic_element_id": id, "project_root": project_root,
                "content_fingerprint": fingerprint,
                "vector": {"engine_id": embedded.engine_id, "model": embedded.model,
                    "dimensions": vector.len(), "vector": vector, "normalized": false,
                    "metadata": {}}});
            (id, record)
        })
        .collect();
    store_page_records(context, project_root, path, records, report);
    Ok(())
}

/// Stores a page in one call. If that call is rejected, each record is retried alone so
/// only the rejected sections are skipped and the rest of the page keeps its vectors.
fn store_page_records(
    context: &mut PluginContext<'_>,
    project_root: &str,
    path: &str,
    records: Vec<(String, Value)>,
    report: &mut TextVectorReport,
) {
    let batch = records.iter().map(|(_, record)| record.clone()).collect();
    match storage::store_element_text_vectors(context, project_root, batch) {
        Ok(stored) => report.stored += stored,
        Err(_) => {
            for (id, record) in records {
                match storage::store_element_text_vectors(context, project_root, vec![record]) {
                    Ok(stored) => report.stored += stored,
                    Err(error) => {
                        report.skipped_sections += 1;
                        report.failures.push(SkippedText {
                            path: path.to_owned(),
                            semantic_element_id: Some(id),
                            error: error.message,
                        });
                    }
                }
            }
        }
    }
}

fn read_page(
    context: &mut PluginContext<'_>,
    project_root: &str,
    path: &str,
) -> Result<Vec<lumvise_project_indexer::HtmlSectionText>, PluginError> {
    let output = context.host_call(
        "project.source",
        json!({"operation": "read", "project_root": project_root, "path": path,
            "start_line": 1, "end_line": WHOLE_FILE_END_LINE}),
    )?;
    let text = output["text"].as_str().ok_or_else(|| {
        PluginError::new(
            "invalid_project_source",
            format!("project.source returned no text for `{path}`"),
            false,
        )
    })?;
    lumvise_project_indexer::html_section_texts(text)
        .map_err(|error| PluginError::new("html_section_parse_failed", error.to_string(), false))
}
