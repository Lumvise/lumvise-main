//! Embeds the visible text of `html_section` elements.
//!
//! The page file is read through `project.source`, the project indexer extracts each
//! section's words with the fingerprint it indexed, and the words are embedded and dropped.
//! Only the vector and fingerprint reach storage: the text is never persisted.
//!
//! Pages are processed one at a time, so one unreadable page or rejected section is
//! reported in the returned report and never fails the caller.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::Serialize;
use serde_json::{Value, json};

use crate::models::SemanticElement;
use crate::query::{EmbedTask, embed_batch_ranges, embed_texts};
use crate::storage;

/// Words per embedded chunk. shortcut: 700 whitespace words approximates the model's ~1,024-token window, upgrade to counting with the model's tokenizer when chunk edges matter.
const CHUNK_WORDS: usize = 700;
/// Largest chunk text sent to `neural.embed`, the host's per-text limit.
const MAX_CHUNK_BYTES: usize = 32 * 1024;

/// `project.source` reads a line range; this end line reads to the end of any real page.
const WHOLE_FILE_END_LINE: u64 = u32::MAX as u64;

/// What the text-vector pass stored and what it could not, with the causes.
#[derive(Debug, Default, Serialize)]
pub(crate) struct TextVectorReport {
    /// Chunk vectors stored.
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

/// The text-vector pass result: what was stored, and the pages it did not start in time.
#[derive(Debug, Default)]
pub(crate) struct TextPass {
    pub report: TextVectorReport,
    /// Pages not started because the time budget ran out; a later call can pass them as `paths`.
    pub pending_paths: Vec<String>,
}

/// Stores text vectors for the active `html_section` elements in `elements`.
///
/// Only pages in `paths` are considered when given. Each page is read once. A page is
/// started only while `may_start_page` returns true; the rest are returned as pending.
/// A section gets a vector only when its indexed fingerprint matches a section the
/// indexer finds in the current page bytes; sections without words get none.
pub(crate) fn index_html_sections(
    context: &mut PluginContext<'_>,
    project_root: &str,
    elements: &[SemanticElement],
    paths: Option<&[String]>,
    may_start_page: impl FnMut() -> bool,
) -> TextPass {
    let mut by_path: BTreeMap<&str, Vec<&SemanticElement>> = BTreeMap::new();
    for element in elements
        .iter()
        .filter(|element| element.element_kind == "html_section" && element.lifecycle == "active")
        .filter(|element| paths.is_none_or(|paths| paths.contains(&element.path)))
    {
        by_path
            .entry(element.path.as_str())
            .or_default()
            .push(element);
    }
    let mut report = TextVectorReport::default();
    let pages = by_path.into_iter().collect::<Vec<_>>();
    let pending = run_within_budget(pages, may_start_page, |(path, page_elements)| {
        if let Err(error) = index_page(context, project_root, path, &page_elements, &mut report) {
            report.skipped_pages += 1;
            report.skipped_sections += page_elements.len();
            report.failures.push(SkippedText {
                path: path.to_owned(),
                semantic_element_id: None,
                error: error.message,
            });
        }
    });
    TextPass {
        report,
        pending_paths: pending
            .into_iter()
            .map(|(path, _)| path.to_owned())
            .collect(),
    }
}

/// Runs `run` on each item while `may_start` allows, and returns the items not started.
/// Once `may_start` says no, it is not asked again: every later item stays pending.
fn run_within_budget<T>(
    items: Vec<T>,
    mut may_start: impl FnMut() -> bool,
    mut run: impl FnMut(T),
) -> Vec<T> {
    let mut items = items.into_iter();
    while let Some(item) = items.next() {
        if !may_start() {
            return std::iter::once(item).chain(items).collect();
        }
        run(item);
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::{MAX_CHUNK_BYTES, run_within_budget, section_chunks};

    #[test]
    fn section_chunks_split_after_700_words() {
        let text = (1..=701)
            .map(|word| format!("w{word}"))
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = section_chunks(&text);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].split_whitespace().count(), 700);
        assert!(chunks[0].ends_with(" w700"));
        assert_eq!(chunks[1], "w701");
    }

    #[test]
    fn empty_and_whitespace_sections_have_no_chunks() {
        assert!(section_chunks("").is_empty());
        assert!(section_chunks(" \n\t  ").is_empty());
    }

    #[test]
    fn a_word_over_the_byte_limit_is_cut_on_char_boundaries() {
        // Three bytes per char and a limit that is not a multiple of three: each cut must back up.
        let word = "€".repeat(MAX_CHUNK_BYTES);
        let chunks = section_chunks(&word);
        assert!(chunks.len() >= 3);
        assert!(chunks.iter().all(|chunk| chunk.len() <= MAX_CHUNK_BYTES));
        assert_eq!(chunks.concat(), word);
    }

    #[test]
    fn a_chunk_of_many_long_words_is_capped_by_bytes_not_only_by_words() {
        let text = vec!["x".repeat(100); 700].join(" ");
        let chunks = section_chunks(&text);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.len() <= MAX_CHUNK_BYTES));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn multibyte_words_count_as_words() {
        let text = vec!["語"; 1400].join(" ");
        let chunks = section_chunks(&text);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].split_whitespace().count(), 700);
        assert_eq!(chunks[1].split_whitespace().count(), 700);
    }

    #[test]
    fn pages_after_the_budget_ends_are_returned_in_order() {
        let mut allowed_starts = 2;
        let mut started = Vec::new();
        let pending = run_within_budget(
            vec!["a", "b", "c", "d"],
            || {
                allowed_starts -= 1;
                allowed_starts >= 0
            },
            |page| started.push(page),
        );
        assert_eq!(started, ["a", "b"]);
        assert_eq!(pending, ["c", "d"]);
    }

    #[test]
    fn no_page_is_pending_when_the_budget_never_ends() {
        let mut started = Vec::new();
        let pending = run_within_budget(vec![1, 2, 3], || true, |page| started.push(page));
        assert_eq!(started, [1, 2, 3]);
        assert!(pending.is_empty());
    }
}

/// One section's chunks, in page order. Chunk `i` of the section is `chunks[i]`.
struct SectionChunks {
    semantic_element_id: String,
    content_fingerprint: String,
    chunks: Vec<String>,
}

/// A chunk vector and the engine that produced it.
struct EmbeddedChunk {
    vector: Vec<f32>,
    engine_id: String,
    model: Option<String>,
}

/// Embeds and stores one page. A section whose chunks cannot be embedded or stored is
/// skipped and counted; an error means the page could not be read or parsed at all.
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
    let mut sections: Vec<SectionChunks> = Vec::new();
    for element in page_elements {
        let Some(fingerprint) = element.content_fingerprint.as_deref() else {
            continue;
        };
        match page_texts.get(fingerprint).map(|text| text.trim()) {
            Some(text) if !text.is_empty() => sections.push(SectionChunks {
                semantic_element_id: element.semantic_element_id.clone(),
                content_fingerprint: fingerprint.to_owned(),
                chunks: section_chunks(text),
            }),
            _ => {}
        }
    }
    // A section's chunks are contiguous in `texts`, so each section owns one range of slots.
    let mut texts: Vec<&str> = Vec::new();
    let mut slots: Vec<Range<usize>> = Vec::with_capacity(sections.len());
    for section in &sections {
        let start = texts.len();
        texts.extend(section.chunks.iter().map(String::as_str));
        slots.push(start..texts.len());
    }
    if texts.is_empty() {
        return Ok(());
    }
    let mut vectors: Vec<Option<EmbeddedChunk>> = (0..texts.len()).map(|_| None).collect();
    let mut embed_errors: Vec<Option<String>> = vec![None; sections.len()];
    for batch in embed_batch_ranges(&texts) {
        if embed_into(context, &texts, batch.clone(), &mut vectors).is_ok() {
            continue;
        }
        // A rejected batch is retried one section at a time, so only the rejected sections are skipped.
        for (index, section_slots) in slots.iter().enumerate() {
            if section_slots.start < batch.end && batch.start < section_slots.end {
                if let Err(error) = embed_into(context, &texts, section_slots.clone(), &mut vectors)
                {
                    embed_errors[index] = Some(error.message);
                }
            }
        }
    }
    let mut records: Vec<(String, Vec<Value>)> = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        let embedded = vectors[slots[index].clone()]
            .iter()
            .map(Option::as_ref)
            .collect::<Option<Vec<_>>>();
        match (&embed_errors[index], embedded) {
            (None, Some(embedded)) => {
                let rows = embedded
                    .iter()
                    .enumerate()
                    .map(|(chunk, embedded)| {
                        json!({"semantic_element_id": section.semantic_element_id,
                            "project_root": project_root,
                            "content_fingerprint": section.content_fingerprint,
                            "chunk": chunk,
                            "vector": {"engine_id": embedded.engine_id, "model": embedded.model,
                                "dimensions": embedded.vector.len(), "vector": embedded.vector,
                                "normalized": false, "metadata": {}}})
                    })
                    .collect();
                records.push((section.semantic_element_id.clone(), rows));
            }
            (Some(error), _) => {
                skip_section(report, path, &section.semantic_element_id, error.clone())
            }
            (None, None) => skip_section(
                report,
                path,
                &section.semantic_element_id,
                "a chunk has no vector".into(),
            ),
        }
    }
    store_page_records(context, project_root, path, records, report);
    Ok(())
}

/// Embeds `texts[range]` in one call and fills the matching `vectors` slots.
fn embed_into(
    context: &mut PluginContext<'_>,
    texts: &[&str],
    range: Range<usize>,
    vectors: &mut [Option<EmbeddedChunk>],
) -> Result<(), PluginError> {
    let batch = embed_texts(context, &texts[range.clone()], EmbedTask::Document)?;
    for (slot, vector) in vectors[range].iter_mut().zip(batch.vectors) {
        *slot = Some(EmbeddedChunk {
            vector,
            engine_id: batch.engine_id.clone(),
            model: batch.model.clone(),
        });
    }
    Ok(())
}

/// Splits section text into chunks of at most `CHUNK_WORDS` words, each cut to `MAX_CHUNK_BYTES`.
fn section_chunks(text: &str) -> Vec<String> {
    let words = text.split_whitespace().collect::<Vec<_>>();
    words
        .chunks(CHUNK_WORDS)
        .flat_map(|group| cut_to_bytes(&group.join(" ")))
        .collect()
}

/// Cuts `text` into pieces of at most `MAX_CHUNK_BYTES`, each ending on a char boundary.
fn cut_to_bytes(text: &str) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut rest = text;
    while rest.len() > MAX_CHUNK_BYTES {
        let mut end = MAX_CHUNK_BYTES;
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        pieces.push(rest[..end].to_owned());
        rest = &rest[end..];
    }
    pieces.push(rest.to_owned());
    pieces
}

fn skip_section(report: &mut TextVectorReport, path: &str, id: &str, error: String) {
    report.skipped_sections += 1;
    report.failures.push(SkippedText {
        path: path.to_owned(),
        semantic_element_id: Some(id.to_owned()),
        error,
    });
}

/// Stores a page in one call. If that call is rejected, each section is retried alone so
/// only the rejected sections are skipped and the rest of the page keeps its vectors.
fn store_page_records(
    context: &mut PluginContext<'_>,
    project_root: &str,
    path: &str,
    records: Vec<(String, Vec<Value>)>,
    report: &mut TextVectorReport,
) {
    let batch = records
        .iter()
        .flat_map(|(_, rows)| rows.iter().cloned())
        .collect();
    match storage::store_element_text_vectors(context, project_root, batch) {
        Ok(stored) => report.stored += stored,
        Err(_) => {
            for (id, rows) in records {
                match storage::store_element_text_vectors(context, project_root, rows) {
                    Ok(stored) => report.stored += stored,
                    Err(error) => skip_section(report, path, &id, error.message),
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
