use std::collections::HashSet;

use chrono::Utc;
use lumvise_contracts::{ArtifactDependency, ArtifactDependencyTarget};
use lumvise_plugin_sdk::PluginContext;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    KnowledgeArtifact, KnowledgeKind,
    semantic_context::{SemanticArtifact, SemanticElement, SemanticRelationship},
};

pub(crate) const UNRESOLVED_FUNCTIONAL_JOB: &str =
    "Functional job unresolved; semantic cultivation required.";
const LEGACY_PLACEHOLDER_FRAGMENT: &str =
    "the named behavior using its inputs and local project context";
const UNRESOLVED_JOB_FRAGMENT: &str = "functional job unresolved; semantic cultivation required";

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct FunctionalSummary {
    pub job: String,
    pub source_interface: String,
    pub receives: Vec<String>,
    pub outcome: String,
    pub effects: Vec<String>,
}

/// Deterministic id of the functional artifact belonging to one element.
pub(crate) fn artifact_id_for(element: &SemanticElement) -> String {
    format!(
        "knowledge-cultivation-functional-{}",
        safe_id(&element.semantic_element_id)
    )
}

/// The stored artifact is still exactly what this element would materialize
/// to, so a cultivation run can skip re-deriving it — including the per-leaf
/// LLM call — and skip the rewrite. Freshness is input-based: the provenance
/// recorded at build time must match the element's current content
/// fingerprint, touching-relationship count and the requested mode.
pub(crate) fn reusable_existing(
    mode: &str,
    element: &SemanticElement,
    touching: usize,
    existing: Option<&KnowledgeArtifact>,
) -> Option<KnowledgeArtifact> {
    let existing = existing.filter(|item| is_usable(item))?;
    if existing.metadata["cultivation"]["mode"].as_str() != Some(mode) {
        return None;
    }
    let provenance = &existing.metadata["provenance"];
    if provenance["content_fingerprint"] != json!(element.content_fingerprint) {
        return None;
    }
    if provenance["relationship_count"].as_u64() != Some(touching as u64) {
        return None;
    }
    Some(existing.clone())
}

pub(crate) fn leaf_artifact(
    mode: &str,
    element: &SemanticElement,
    relationships: &[SemanticRelationship],
    indexed_artifacts: &[SemanticArtifact],
    context: &mut PluginContext<'_>,
) -> KnowledgeArtifact {
    let source = source_text(element, indexed_artifacts);
    let inferred = functional_summary(element, &source);
    let summary = llm_summary(element, &source, context).unwrap_or(inferred);
    artifact(mode, element, relationships, &summary)
}

pub(crate) fn composite_artifact(
    mode: &str,
    element: &SemanticElement,
    relationships: &[SemanticRelationship],
    children: &[KnowledgeArtifact],
) -> KnowledgeArtifact {
    let resolved = children
        .iter()
        .filter(|item| resolved_job(item).is_some())
        .collect::<Vec<_>>();
    let summary = FunctionalSummary {
        job: aggregate_functional_jobs(resolved.iter().filter_map(|item| job(item))),
        source_interface: joined_field(&resolved, "source_interface", unresolved_interface()),
        receives: array_field(&resolved, "receives", unresolved_receives()),
        outcome: joined_field(&resolved, "outcome", unresolved_outcome()),
        effects: array_field(&resolved, "effects", unresolved_effects()),
    };
    let mut artifact = artifact(mode, element, relationships, &summary);
    artifact.dependencies = resolved
        .iter()
        .map(|child| ArtifactDependency {
            target: ArtifactDependencyTarget::Artifact {
                artifact_id: child.artifact_id.clone(),
            },
        })
        .collect();
    artifact.metadata["cultivation"]["derived_from"] =
        json!("immediate_child_functional_artifacts");
    artifact
}

pub(crate) fn generated_artifact(
    element: &SemanticElement,
    relationships: &[SemanticRelationship],
    summary: &FunctionalSummary,
) -> KnowledgeArtifact {
    artifact("mcp_local_llm", element, relationships, summary)
}

pub(crate) fn is_usable(artifact: &KnowledgeArtifact) -> bool {
    artifact
        .tags
        .iter()
        .any(|tag| tag == "cultivation-functional")
        && job(artifact).is_some_and(|value| !is_unresolved_functional_job(value))
}

pub(crate) fn functional_content_hash(artifact: &KnowledgeArtifact) -> String {
    let signature = json!({
        "content": artifact.content, "job": artifact.metadata["job"],
        "source_interface": artifact.metadata["source_interface"],
        "receives": artifact.metadata["receives"], "receivers": artifact.metadata["receivers"],
        "outcome": artifact.metadata["outcome"], "outtakes": artifact.metadata["outtakes"],
        "effects": artifact.metadata["effects"]
    });
    stable_content_hash(&signature.to_string())
}

pub(crate) fn stable_content_hash(content: &str) -> String {
    let digest = content
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    format!("fnv1a64:{digest:016x}")
}

pub(crate) fn is_unresolved_functional_job(job: &str) -> bool {
    let normalized = job.trim().to_ascii_lowercase();
    normalized.is_empty()
        || normalized.contains(UNRESOLVED_JOB_FRAGMENT)
        || normalized.contains(LEGACY_PLACEHOLDER_FRAGMENT)
}

pub(crate) fn functional_job_status(job: &str) -> &'static str {
    if is_unresolved_functional_job(job) {
        "unresolved"
    } else {
        "resolved"
    }
}

pub(crate) fn aggregate_functional_jobs<'a>(jobs: impl Iterator<Item = &'a str>) -> String {
    let resolved = jobs
        .filter(|job| !is_unresolved_functional_job(job))
        .take(3)
        .collect::<Vec<_>>();
    if resolved.is_empty() {
        UNRESOLVED_FUNCTIONAL_JOB.into()
    } else {
        resolved.join(" ")
    }
}

fn artifact(
    mode: &str,
    element: &SemanticElement,
    relationships: &[SemanticRelationship],
    summary: &FunctionalSummary,
) -> KnowledgeArtifact {
    KnowledgeArtifact {
        artifact_id: artifact_id_for(element),
        semantic_element_id: element.semantic_element_id.clone(),
        knowledge_type: KnowledgeKind::DerivedSummary,
        title: format!("Functional meaning: {}", element.name),
        content: functional_content(summary),
        tags: vec!["cultivation-functional".into()],
        dependencies: Vec::new(),
        metadata: functional_metadata(mode, element, relationships, summary),
        path: None,
        project_root: Some(element.project_root.clone()),
    }
}

fn functional_metadata(
    mode: &str,
    element: &SemanticElement,
    relationships: &[SemanticRelationship],
    summary: &FunctionalSummary,
) -> Value {
    json!({
        "job": summary.job, "job_status": functional_job_status(&summary.job),
        "receives": summary.receives, "outcome": summary.outcome, "effects": summary.effects,
        "source_interface": summary.source_interface, "function": summary.job,
        "transformation": summary.outcome, "c4_level": "C4 L4 code",
        "c4": {"level": 4, "level_name": "code", "parent_id": element.parent_element_id,
            "source_element_ids": [element.semantic_element_id.clone()],
            "generated_from": "semantic_graph+source_signature"},
        "structural_placement": {"project_root": element.project_root,
            "parent_element_id": element.parent_element_id, "path": element.path,
            "element_kind": element.element_kind},
        "provenance": {"semantic_element_id": element.semantic_element_id,
            "content_fingerprint": element.content_fingerprint,
            "relationship_count": touching_relationships(element, relationships)},
        "cultivation": {"mode": mode, "generated_at": Utc::now().to_rfc3339()}
    })
}

fn touching_relationships(
    element: &SemanticElement,
    relationships: &[SemanticRelationship],
) -> usize {
    relationships
        .iter()
        .filter(|item| {
            item.source_element_id == element.semantic_element_id
                || item.target_element_id == element.semantic_element_id
        })
        .count()
}

fn functional_content(summary: &FunctionalSummary) -> String {
    format!(
        "## Job\n{}\n\n## Source Interface\n{}\n\n## Receives\n{}\n\n## Outcome\n{}\n\n## Notable Effects\n{}",
        summary.job,
        summary.source_interface,
        markdown_items(&summary.receives),
        summary.outcome,
        markdown_items(&summary.effects)
    )
}

fn functional_summary(element: &SemanticElement, source: &str) -> FunctionalSummary {
    let snippet = source
        .lines()
        .take(80)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let signature = source_signature(element, &snippet);
    FunctionalSummary {
        job: job_text(element, &snippet),
        source_interface: signature.clone(),
        receives: receives(&signature, element),
        outcome: outcome_text(&signature),
        effects: effect_texts(&snippet),
    }
}

#[cfg(test)]
pub(crate) fn golden_summary(element: &SemanticElement, source: &str) -> Value {
    let summary = functional_summary(element, source);
    json!({
        "job": summary.job,
        "source_interface": summary.source_interface,
        "receives": summary.receives,
        "outcome": summary.outcome,
        "effects": summary.effects
    })
}

fn source_text(element: &SemanticElement, artifacts: &[SemanticArtifact]) -> String {
    artifacts
        .iter()
        .find(|item| {
            item.semantic_element_id == element.semantic_element_id && item.content.is_some()
        })
        .and_then(|item| item.content.clone())
        .or_else(|| metadata_signature(element))
        .unwrap_or_default()
}

fn metadata_signature(element: &SemanticElement) -> Option<String> {
    element.metadata["indexer_metadata"]["signature"]
        .as_str()
        .or_else(|| element.metadata["signature"].as_str())
        .map(str::to_owned)
}

fn source_signature(element: &SemanticElement, snippet: &[String]) -> String {
    let signature = signature_lines(snippet);
    if signature.is_empty() {
        format!(
            "{} `{}` in `{}`.",
            element.element_kind, element.name, element.path
        )
    } else {
        signature
    }
}

fn signature_lines(snippet: &[String]) -> String {
    snippet
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .scan(String::new(), |state, line| {
            state.push_str(line);
            state.push(' ');
            Some(state.trim().to_owned())
        })
        .find(|line| line.contains('{') || line.ends_with(';') || balanced_signature(line))
        .unwrap_or_default()
        .trim_end_matches('{')
        .trim()
        .to_owned()
}

fn balanced_signature(signature: &str) -> bool {
    signature.contains('(') && signature.matches('(').count() <= signature.matches(')').count()
}

fn receives(signature: &str, element: &SemanticElement) -> Vec<String> {
    let parameters = signature_parameters(signature);
    if parameters.is_empty() {
        vec![format!(
            "No explicit parameters are indexed; uses local context from `{}`.",
            element.path
        )]
    } else {
        parameters
    }
}

pub(crate) fn signature_parameters(signature: &str) -> Vec<String> {
    let Some(start) = function_parameter_start(signature) else {
        return Vec::new();
    };
    let Some(end) = matching_close_paren(signature, start) else {
        return Vec::new();
    };
    split_parameters(&signature[start + 1..end])
}

fn function_parameter_start(signature: &str) -> Option<usize> {
    let index = signature.find("fn ")?;
    signature[index..].find('(').map(|offset| index + offset)
}

fn matching_close_paren(signature: &str, start: usize) -> Option<usize> {
    let mut depth = 0_i64;
    for (index, item) in signature
        .char_indices()
        .skip_while(|(index, _)| *index < start)
    {
        depth += i64::from(item == '(');
        depth -= i64::from(item == ')');
        if depth == 0 && index > start {
            return Some(index);
        }
    }
    None
}

fn split_parameters(params: &str) -> Vec<String> {
    top_level_comma_segments(params)
        .into_iter()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(parameter_text)
        .collect()
}

fn top_level_comma_segments(value: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let (mut start, mut paren, mut bracket, mut brace, mut angle) = (0, 0, 0, 0, 0);
    for (index, item) in value.char_indices() {
        observe_nesting(item, &mut paren, &mut bracket, &mut brace, &mut angle);
        if item == ',' && paren == 0 && bracket == 0 && brace == 0 && angle == 0 {
            segments.push(&value[start..index]);
            start = index + 1;
        }
    }
    segments.push(&value[start..]);
    segments
}

fn observe_nesting(
    item: char,
    paren: &mut i32,
    bracket: &mut i32,
    brace: &mut i32,
    angle: &mut i32,
) {
    match item {
        '(' => *paren += 1,
        ')' => *paren -= 1,
        '[' => *bracket += 1,
        ']' => *bracket -= 1,
        '{' => *brace += 1,
        '}' => *brace -= 1,
        '<' => *angle += 1,
        '>' if *angle > 0 => *angle -= 1,
        _ => {}
    }
}

fn parameter_text(parameter: &str) -> String {
    if matches!(parameter, "&self" | "self" | "&mut self") {
        format!("The `{parameter}` receiver state.")
    } else {
        format!("Parameter `{parameter}`.")
    }
}

pub(crate) fn outcome_text(signature: &str) -> String {
    let Some(return_type) = return_type(signature) else {
        return "Completes for side effects or mutates state without returning a value.".into();
    };
    if return_type.starts_with("Result") {
        return format!("Returns `{return_type}`, carrying the success value or an error.");
    }
    if return_type.starts_with("Option") {
        return format!("Returns `{return_type}`, so the result may be absent.");
    }
    format!("Returns `{return_type}`.")
}

fn return_type(signature: &str) -> Option<String> {
    let marker = signature.find("->")?;
    let value = signature[marker + 2..]
        .split(" where ")
        .next()
        .unwrap_or_default()
        .trim()
        .trim_end_matches('{')
        .trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn job_text(element: &SemanticElement, snippet: &[String]) -> String {
    let body = snippet.join("\n").to_ascii_lowercase();
    format!(
        "{} `{}`: {}.",
        capitalize(&element.element_kind),
        element.name,
        job_phrase(action_from_name(&element.name), &body)
    )
}

fn action_from_name(name: &str) -> &str {
    match name.split('_').next().unwrap_or(name) {
        "get" | "fetch" | "load" | "read" | "find" | "list" => "retrieves",
        "write" | "save" | "store" | "upsert" | "insert" | "create" => "persists",
        "build" | "format" | "render" | "compose" | "response" => "builds",
        "parse" | "decode" | "deserialize" | "encode" | "serialize" => "converts",
        "validate" | "ensure" | "require" | "check" => "validates",
        "handle" | "process" | "run" | "execute" | "invoke" => "orchestrates",
        "delete" | "remove" | "clear" => "removes",
        _ => "implements",
    }
}

fn job_phrase(action: &str, body: &str) -> String {
    if body.contains("pub enum") {
        return "defines typed variants and messages for callers".into();
    }
    if body.contains("pub type") && body.contains("std::result::result") && body.contains("dberror")
    {
        return "aliases fallible database operations to the project error type".into();
    }
    if body.contains("self::") || body.contains("-> self") {
        return "constructs typed enum values from provided context".into();
    }
    if body.contains("plugin_mcp_bridge_response") && body.contains("plugin_http_route") {
        return "checks bridge-owned plugin endpoints first, then resolves registered plugin HTTP routes and dispatches matching requests".into();
    }
    if body.contains("knowledge_plugin_response") {
        return "routes a resolved plugin capability to the Knowledge HTTP response handler or reports unsupported capabilities".into();
    }
    if body.contains("httpresponse") || body.contains("json_response") {
        return format!("{action} an HTTP/API response from the provided request context");
    }
    if body.contains("serde_json") || body.contains("json!(") {
        return format!("{action} structured JSON data for callers");
    }
    if body.contains("upsert") || body.contains("insert") || body.contains("write") {
        return format!("{action} data into the owned storage boundary");
    }
    UNRESOLVED_FUNCTIONAL_JOB.into()
}

fn effect_texts(snippet: &[String]) -> Vec<String> {
    let body = snippet.join("\n").to_ascii_lowercase();
    let candidates = [
        (
            body.contains('?'),
            "Propagates fallible operations to the caller.",
        ),
        (
            body.contains("json"),
            "Serializes or shapes structured data.",
        ),
        (
            body.contains("http"),
            "Interacts with HTTP request/response flow.",
        ),
        (
            body.contains("request.query"),
            "Reads query parameters from the incoming request.",
        ),
        (
            body.contains("bridge_response"),
            "Normalizes projection success/error results into a bridge response.",
        ),
        (
            body.contains("upsert") || body.contains("insert"),
            "Persists or updates stored records.",
        ),
        (
            body.contains("read"),
            "Reads from local state, storage, or input content.",
        ),
        (
            body.contains("write") || body.contains("create"),
            "Writes or creates output/state.",
        ),
    ];
    let effects = candidates
        .into_iter()
        .filter(|(yes, _)| *yes)
        .map(|(_, text)| text.into())
        .collect::<Vec<_>>();
    if effects.is_empty() {
        vec!["No obvious external side effect is visible from the indexed source span.".into()]
    } else {
        effects
    }
}

fn llm_summary(
    element: &SemanticElement,
    source: &str,
    context: &mut PluginContext<'_>,
) -> Option<FunctionalSummary> {
    if source.trim().is_empty() {
        return None;
    }
    let prompt = format!(
        "Return JSON with job, source_interface, receives, outcome, effects for `{}`:\n{}",
        element.name, source
    );
    let output = context.host_call("neural.llm", json!({
        "provider_id": null, "model": null, "conversation_id": null, "llm_session_id": null,
        "messages": [{"role": "system", "content": "Infer precise functional meaning from source."},
            {"role": "user", "content": prompt}], "mcp_servers": []
    })).ok()?;
    let response = output.get("response").unwrap_or(&output);
    let content = response["content"].as_str()?;
    serde_json::from_str(content).ok()
}

fn job(artifact: &KnowledgeArtifact) -> Option<&str> {
    artifact.metadata["job"]
        .as_str()
        .or_else(|| section(&artifact.content, "Job"))
}

fn resolved_job(artifact: &KnowledgeArtifact) -> Option<&str> {
    job(artifact).filter(|value| !is_unresolved_functional_job(value))
}

fn joined_field(artifacts: &[&KnowledgeArtifact], field: &str, fallback: &str) -> String {
    let values = unique_strings(
        artifacts
            .iter()
            .filter_map(|item| string_field(item, field)),
    );
    if values.is_empty() {
        fallback.into()
    } else {
        values.join(" ")
    }
}

fn array_field(artifacts: &[&KnowledgeArtifact], field: &str, fallback: &str) -> Vec<String> {
    let values = artifacts
        .iter()
        .flat_map(|item| array_values(item, field))
        .collect::<Vec<_>>();
    let unique = unique_strings(values.iter().map(String::as_str));
    if unique.is_empty() {
        vec![fallback.into()]
    } else {
        unique
    }
}

fn string_field<'a>(artifact: &'a KnowledgeArtifact, field: &str) -> Option<&'a str> {
    artifact.metadata[field]
        .as_str()
        .or_else(|| section(&artifact.content, heading(field)))
}

fn array_values(artifact: &KnowledgeArtifact, field: &str) -> Vec<String> {
    artifact.metadata[field]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_else(|| section_items(&artifact.content, heading(field)))
}

fn heading(field: &str) -> &str {
    match field {
        "job" => "Job",
        "source_interface" => "Source Interface",
        "receives" => "Receives",
        "outcome" => "Outcome",
        "effects" => "Notable Effects",
        _ => field,
    }
}

fn section<'a>(content: &'a str, heading: &str) -> Option<&'a str> {
    let marker = format!("## {heading}\n");
    let start = content.find(&marker)? + marker.len();
    let rest = &content[start..];
    let end = rest.find("\n\n## ").unwrap_or(rest.len());
    let value = rest[..end].trim();
    (!value.is_empty()).then_some(value)
}

fn section_items(content: &str, heading: &str) -> Vec<String> {
    section(content, heading)
        .into_iter()
        .flat_map(str::lines)
        .map(|line| line.trim().trim_start_matches("- ").to_owned())
        .filter(|line| !line.is_empty())
        .collect()
}

fn unique_strings<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .filter(|value| !value.trim().is_empty())
        .filter(|value| seen.insert((*value).to_owned()))
        .map(str::to_owned)
        .collect()
}

fn markdown_items(items: &[String]) -> String {
    if items.is_empty() {
        "- None declared by immediate child artifacts.".into()
    } else {
        items
            .iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn unresolved_interface() -> &'static str {
    "Functional source interface unresolved; semantic cultivation required."
}
fn unresolved_receives() -> &'static str {
    "Functional inputs unresolved; semantic cultivation required."
}
fn unresolved_outcome() -> &'static str {
    "Functional outcome unresolved; semantic cultivation required."
}
fn unresolved_effects() -> &'static str {
    "Functional effects unresolved; semantic cultivation required."
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    chars.next().map_or_else(String::new, |first| {
        format!(
            "{}{}",
            first.to_ascii_uppercase(),
            chars.collect::<String>()
        )
    })
}

fn safe_id(value: &str) -> String {
    value
        .chars()
        .map(|item| {
            if item.is_ascii_alphanumeric() {
                item
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_parameters_keep_generic_commas_together() {
        let values = signature_parameters("pub fn map(value: Result<A, B>, pair: (C, D)) -> E");
        assert_eq!(
            values,
            vec![
                "Parameter `value: Result<A, B>`.",
                "Parameter `pair: (C, D)`."
            ]
        );
    }

    #[test]
    fn outcome_text_handles_pub_crate_signatures() {
        assert_eq!(
            outcome_text("pub(crate) fn load() -> Result<Value, Error>"),
            "Returns `Result<Value, Error>`, carrying the success value or an error."
        );
    }

    #[test]
    fn aggregate_excludes_unresolved_jobs() {
        let jobs = [UNRESOLVED_FUNCTIONAL_JOB, "Persists one value."];
        assert_eq!(
            aggregate_functional_jobs(jobs.into_iter()),
            "Persists one value."
        );
    }
}
