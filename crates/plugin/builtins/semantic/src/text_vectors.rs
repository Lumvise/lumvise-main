//! Owns HTML section text extraction and exact token-planned embedding.
//! Only vectors/fingerprints leave this module for persistence; source remains transient.
use crate::{
    models::SemanticElement,
    query::{EmbedTask, embed_texts_guarded},
    storage,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

const WHOLE_FILE_END_LINE: u64 = u32::MAX as u64;
const SECTION_TOKEN_BUDGET: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct EmbeddingIdentity {
    pub engine_id: String,
    pub model: Option<String>,
    pub profile_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct SourceRange {
    pub start_byte: usize,
    pub end_byte: usize,
    pub token_count: usize,
}

#[derive(Debug, Deserialize)]
struct TokenPlan {
    #[serde(flatten)]
    identity: EmbeddingIdentity,
    ranges: Vec<SourceRange>,
}

pub(crate) struct PlannedSection {
    pub text: String,
    pub identity: EmbeddingIdentity,
    pub ranges: Vec<SourceRange>,
}

#[derive(Default)]
pub(crate) struct SourcePages {
    pages: BTreeMap<String, Vec<lumvise_project_indexer::HtmlSectionText>>,
}

impl SourcePages {
    pub(crate) fn prepare(
        &mut self,
        context: &mut PluginContext<'_>,
        root: &str,
        path: &str,
    ) -> Result<(), PluginError> {
        if !self.pages.contains_key(path) {
            self.pages
                .insert(path.into(), read_page(context, root, path)?);
        }
        Ok(())
    }

    pub(crate) fn section(
        &mut self,
        context: &mut PluginContext<'_>,
        root: &str,
        element: &SemanticElement,
    ) -> Result<PlannedSection, PluginError> {
        self.prepare(context, root, &element.path)?;
        let text = self.pages[&element.path]
            .iter()
            .find(|section| {
                Some(section.content_fingerprint.as_str()) == element.content_fingerprint.as_deref()
            })
            .ok_or_else(|| {
                source_conflict("current source section does not match indexed fingerprint")
            })?
            .text
            .clone();
        plan_section(context, text)
    }
}

pub(crate) fn plan_identity(
    context: &mut PluginContext<'_>,
    text: &str,
) -> Result<EmbeddingIdentity, PluginError> {
    Ok(plan_section(context, text.to_owned())?.identity)
}

fn plan_section(
    context: &mut PluginContext<'_>,
    text: String,
) -> Result<PlannedSection, PluginError> {
    let plan = request_token_plan(context, &text)?;
    validate_ranges(&text, &plan.ranges)?;
    if plan.identity.profile_fingerprint.is_empty() || plan.identity.engine_id.is_empty() {
        return Err(source_conflict(
            "expected nonempty embedding profile and engine identity",
        ));
    }
    Ok(PlannedSection {
        text,
        identity: plan.identity,
        ranges: plan.ranges,
    })
}

fn request_token_plan(
    context: &mut PluginContext<'_>,
    text: &str,
) -> Result<TokenPlan, PluginError> {
    let output = context.host_call(
        "neural.chunk_text",
        json!({"text":text,"task":"document","max_tokens":SECTION_TOKEN_BUDGET}),
    )?;
    serde_json::from_value(output)
        .map_err(|_| source_conflict("expected exact token ranges and attested embedding identity"))
}

fn validate_ranges(text: &str, ranges: &[SourceRange]) -> Result<(), PluginError> {
    if text.trim().is_empty() && ranges.is_empty() {
        return Ok(());
    }
    let mut cursor = 0;
    for range in ranges {
        if !valid_range(text, cursor, range) {
            return Err(source_conflict(
                "expected contiguous UTF-8 ranges covering source within token budget",
            ));
        }
        cursor = range.end_byte;
    }
    if cursor != text.len() {
        return Err(source_conflict(
            "token plan omitted part of original source",
        ));
    }
    Ok(())
}

fn valid_range(text: &str, cursor: usize, range: &SourceRange) -> bool {
    range.start_byte == cursor
        && range.end_byte > cursor
        && range.end_byte <= text.len()
        && text.is_char_boundary(range.end_byte)
        && range.token_count > 0
        && range.token_count <= SECTION_TOKEN_BUDGET
}

pub(crate) fn embed_chunk(
    context: &mut PluginContext<'_>,
    section: &PlannedSection,
    chunk: usize,
) -> Result<Vec<f32>, PluginError> {
    let range = section
        .ranges
        .get(chunk)
        .ok_or_else(|| source_conflict("expected planned chunk ordinal"))?;
    embed_pinned(
        context,
        &section.text[range.start_byte..range.end_byte],
        &section.identity,
    )
}

pub(crate) fn embed_pinned(
    context: &mut PluginContext<'_>,
    text: &str,
    identity: &EmbeddingIdentity,
) -> Result<Vec<f32>, PluginError> {
    let embedded = embed_texts_guarded(
        context,
        &[text],
        EmbedTask::Document,
        Some(&identity.profile_fingerprint),
    )?;
    verify_embedded_identity(&embedded, identity)?;
    embedded
        .vectors
        .into_iter()
        .next()
        .ok_or_else(|| source_conflict("expected one complete chunk vector"))
}

fn verify_embedded_identity(
    embedded: &crate::query::EmbedBatch,
    identity: &EmbeddingIdentity,
) -> Result<(), PluginError> {
    if embedded.engine_id == identity.engine_id && embedded.model == identity.model {
        return Ok(());
    }
    Err(PluginError::new(
        "rebuild_profile_conflict",
        "embedding engine/model changed from pinned source plan",
        true,
    ))
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct TextVectorReport {
    pub stored: usize,
    pub skipped_sections: usize,
    pub skipped_pages: usize,
    pub failures: Vec<SkippedText>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SkippedText {
    pub path: String,
    pub semantic_element_id: Option<String>,
    pub error: String,
}

/// Trigger path publishes each complete section; resumable rebuilds share the same planner.
pub(crate) fn index_html_sections(
    context: &mut PluginContext<'_>,
    root: &str,
    elements: &[SemanticElement],
) -> TextVectorReport {
    let mut report = TextVectorReport::default();
    let mut sources = SourcePages::default();
    for (path, sections) in grouped_sections(elements) {
        publish_page(context, root, path, &sections, &mut sources, &mut report);
    }
    report
}

fn grouped_sections(elements: &[SemanticElement]) -> BTreeMap<&str, Vec<&SemanticElement>> {
    let mut pages: BTreeMap<&str, Vec<&SemanticElement>> = BTreeMap::new();
    for element in elements
        .iter()
        .filter(|element| element.element_kind == "html_section" && element.lifecycle == "active")
    {
        pages.entry(&element.path).or_default().push(element);
    }
    pages
}

fn publish_page(
    context: &mut PluginContext<'_>,
    root: &str,
    path: &str,
    sections: &[&SemanticElement],
    sources: &mut SourcePages,
    report: &mut TextVectorReport,
) {
    if sources.prepare(context, root, path).is_err() {
        record_page_failure(report, path, sections.len());
        return;
    }
    for element in sections {
        match publish_section(context, root, element, sources) {
            Ok(count) => report.stored += count,
            Err(_) => record_section_failure(report, element),
        }
    }
}

fn publish_section(
    context: &mut PluginContext<'_>,
    root: &str,
    element: &SemanticElement,
    sources: &mut SourcePages,
) -> Result<usize, PluginError> {
    let section = sources.section(context, root, element)?;
    let records = complete_section_records(context, root, element, &section)?;
    if records.is_empty() {
        return Ok(0);
    }
    storage::store_element_text_vectors(context, root, records)
}

fn complete_section_records(
    context: &mut PluginContext<'_>,
    root: &str,
    element: &SemanticElement,
    section: &PlannedSection,
) -> Result<Vec<serde_json::Value>, PluginError> {
    let mut records = Vec::with_capacity(section.ranges.len());
    for ordinal in 0..section.ranges.len() {
        let vector = embed_chunk(context, &section, ordinal)?;
        records.push(section_vector_record(
            root,
            element,
            &section.identity,
            ordinal,
            vector,
        ));
    }
    Ok(records)
}

fn section_vector_record(
    root: &str,
    element: &SemanticElement,
    identity: &EmbeddingIdentity,
    ordinal: usize,
    vector: Vec<f32>,
) -> serde_json::Value {
    json!({"project_root":root,"semantic_element_id":element.semantic_element_id,
        "content_fingerprint":element.content_fingerprint,"chunk":ordinal,
        "vector":{"engine_id":identity.engine_id,"model":identity.model,
        "dimensions":vector.len(),"vector":vector,"normalized":false,"metadata":{}}})
}

fn record_page_failure(report: &mut TextVectorReport, path: &str, sections: usize) {
    report.skipped_pages += 1;
    report.skipped_sections += sections;
    report.failures.push(SkippedText {
        path: path.into(),
        semantic_element_id: None,
        error: "Page source could not be read or parsed; refresh source before rebuilding.".into(),
    });
}

fn record_section_failure(report: &mut TextVectorReport, element: &SemanticElement) {
    report.skipped_sections += 1;
    report.failures.push(SkippedText { path: element.path.clone(), semantic_element_id: Some(element.semantic_element_id.clone()),
        error: "Complete section could not be planned, embedded or published; run an explicit resumable rebuild.".into() });
}

fn source_conflict(message: &str) -> PluginError {
    PluginError::new("rebuild_source_conflict", message, true)
}

fn read_page(
    context: &mut PluginContext<'_>,
    root: &str,
    path: &str,
) -> Result<Vec<lumvise_project_indexer::HtmlSectionText>, PluginError> {
    let output = context.host_call("project.source", json!({"operation":"read", "project_root":root, "path":path, "start_line":1, "end_line":WHOLE_FILE_END_LINE}))?;
    let text = output["text"]
        .as_str()
        .ok_or_else(|| source_conflict("project.source returned no page text"))?;
    lumvise_project_indexer::html_section_texts(text)
        .map_err(|_| source_conflict("page could not be parsed into indexed sections"))
}
