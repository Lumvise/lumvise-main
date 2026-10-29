use lumvise_contracts::KnowledgeDocument;
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    cultivation, documents,
    functional::{self, FunctionalSummary},
    semantic_context::SemanticElement,
    storage,
};

mod jobs;
#[cfg(test)]
use jobs::GenerationDocumentBase;
use jobs::{GenerationJobScope, PendingArtifactJob};

const CAPABILITY: &str = "runtime.project_execution";
const GENERATOR: &str = "semantic.generate_functional_artifacts.v1";
const JOBS: &str = "knowledge_functional_artifact_jobs";
const FAILURES: &str = "knowledge_functional_artifact_failures";
/// Project execution is temporarily full; the element stays unsubmitted and a
/// later poll submits it, instead of recording a permanent failure.
const QUOTA_EXCEEDED: &str = "host_capability_quota_exceeded";

/// How eagerly the host should run a submitted generation job.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SubmitPriority {
    /// Direct user request (scoped C4, cultivation tool).
    Interactive,
    /// Background refresh work.
    #[default]
    Background,
}

/// A generation job that keeps disappearing from the host is capped: after
/// `MAX_LOST_RESUBMITS` resubmissions the element dead-letters instead of
/// spinning through submit/lost cycles forever.
const MAX_LOST_RESUBMITS: u32 = 3;

#[derive(Deserialize)]
struct ProjectExecutionJob {
    status: String,
    output: Option<Value>,
    error: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct ArtifactGenerationFailure {
    pub(crate) project_root: String,
    pub(crate) semantic_element_id: String,
    pub(crate) content_fingerprint: String,
    pub(crate) reason_code: String,
    pub(crate) reason: String,
}

impl ArtifactGenerationFailure {
    fn for_pending(pending: &PendingArtifactJob, reason_code: String, reason: String) -> Self {
        Self {
            project_root: pending.project_root.clone(),
            semantic_element_id: pending.semantic_element_id.clone(),
            content_fingerprint: pending.content_fingerprint.clone(),
            reason_code,
            reason,
        }
    }

    fn for_element(element: &SemanticElement, error: &PluginError) -> Self {
        Self {
            project_root: element.project_root.clone(),
            semantic_element_id: element.semantic_element_id.clone(),
            content_fingerprint: element.content_fingerprint.clone().unwrap_or_default(),
            reason_code: error.code.clone(),
            reason: error.message.clone(),
        }
    }
}

pub(crate) struct ArtifactGenerationState {
    pub(crate) pending_element_ids: Vec<String>,
    pub(crate) failures: Vec<ArtifactGenerationFailure>,
}

pub(crate) struct ArtifactSubmission {
    pub(crate) submitted: usize,
    pub(crate) provider_unavailable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobDisposition {
    Pending,
    Succeeded,
    TerminalFailure,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GeneratedFunctionalArtifacts {
    semantic_element_id: String,
    content_fingerprint: String,
    job: String,
    source_interface: String,
    receives: Vec<String>,
    outcome: String,
    effects: Vec<String>,
}

pub(crate) fn submit_missing(
    elements: &[SemanticElement],
    artifacts: &[KnowledgeDocument],
    retry_failed: bool,
    priority: SubmitPriority,
    context: &mut PluginContext<'_>,
) -> Result<ArtifactSubmission, PluginError> {
    if !elements
        .iter()
        .any(|element| needs_generation(element, artifacts))
    {
        return Ok(ArtifactSubmission {
            submitted: 0,
            provider_unavailable: false,
        });
    }
    let scope = GenerationJobScope::current(context)?;
    let sources = crate::source_revisions::SelectedSources::for_documents(
        context,
        &elements[0].project_root,
        artifacts,
    )?;
    let pending = scope.pending(context)?;
    let failures = scope.failures(context)?;
    let mut submitted = 0;
    for element in elements {
        if skip_submission(element, artifacts, &pending, &failures, retry_failed) {
            continue;
        }
        if retry_failed {
            storage::delete(context, FAILURES, &scope.key(element))?;
        }
        match submit(
            element, artifacts, priority, &scope, &pending, &sources, context,
        ) {
            Ok(Some(_job)) => {
                submitted += 1;
            }
            Ok(None) => {}
            Err(error) if error.code == "host_capability_unavailable" => {
                return Ok(ArtifactSubmission {
                    submitted,
                    provider_unavailable: true,
                });
            }
            Err(error) if error.code == QUOTA_EXCEEDED => {
                return Ok(ArtifactSubmission {
                    submitted,
                    provider_unavailable: false,
                });
            }
            Err(error) if documents::is_generation_hold(&error) => {
                scope.hold(element, &error, context)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(ArtifactSubmission {
        submitted,
        provider_unavailable: false,
    })
}

/// Consumes finished jobs. `project_root` limits the sweep to one project so a
/// scoped C4 request does not pay for every project's pending jobs.
pub(crate) fn poll(
    context: &mut PluginContext<'_>,
    project_root: Option<&str>,
) -> Result<Value, PluginError> {
    use std::collections::BTreeMap;

    let scope = GenerationJobScope::current(context)?;
    let pending = scope.pending(context)?;
    let mut completed_by_project = BTreeMap::<String, Vec<PendingArtifactJob>>::new();
    let mut consumption_error = None;
    for mut job in pending
        .into_iter()
        .filter(|job| project_root.is_none_or(|root| job.project_root == root))
    {
        match consume(&mut job, context) {
            Ok(true) => completed_by_project
                .entry(job.project_root.clone())
                .or_default()
                .push(job),
            Ok(false) => {}
            Err(error) => {
                consumption_error = Some(error);
                break;
            }
        }
    }
    let mut completed = 0;
    for (project_root, jobs) in completed_by_project {
        let changed_ids = jobs
            .iter()
            .map(|job| job.semantic_element_id.clone())
            .collect::<Vec<_>>();
        cultivation::refresh_reports_for_changed_elements(&project_root, &changed_ids, context)?;
        for job in jobs {
            job.finish(None, context)?;
            completed += 1;
        }
    }
    if let Some(error) = consumption_error {
        return Err(error);
    }
    Ok(json!({"pending": scope.pending(context)?.len(),
        "completed": completed}))
}

pub(crate) fn state_for(
    elements: &[SemanticElement],
    context: &mut PluginContext<'_>,
) -> Result<ArtifactGenerationState, PluginError> {
    let scope = GenerationJobScope::current(context)?;
    let pending = scope.pending(context)?;
    let failures = scope.failures(context)?;
    let pending_element_ids = matching_element_ids(elements, &pending);
    let failures = failures
        .into_iter()
        .filter(|failure| failure_matches_elements(failure, elements))
        .collect();
    Ok(ArtifactGenerationState {
        pending_element_ids,
        failures,
    })
}

/// Number of active elements that have source text to generate from.
pub(crate) fn generatable_count(elements: &[SemanticElement]) -> usize {
    elements
        .iter()
        .filter(|element| element.lifecycle == "active" && target_input(element).is_some())
        .count()
}

pub(crate) fn missing_element_ids(
    elements: &[SemanticElement],
    artifacts: &[KnowledgeDocument],
) -> Vec<String> {
    elements
        .iter()
        .filter(|element| needs_generation(element, artifacts))
        .map(|element| element.semantic_element_id.clone())
        .collect()
}

fn submit(
    element: &SemanticElement,
    artifacts: &[KnowledgeDocument],
    priority: SubmitPriority,
    scope: &GenerationJobScope,
    pending: &[PendingArtifactJob],
    sources: &crate::source_revisions::SelectedSources,
    context: &mut PluginContext<'_>,
) -> Result<Option<PendingArtifactJob>, PluginError> {
    let draft = match pending.iter().find(|job| current_target(element, job)) {
        Some(draft) => draft.clone(),
        None => {
            let draft =
                PendingArtifactJob::capture(element, artifacts, priority, scope, sources, context)?;
            draft.replace(None, context)?;
            draft
        }
    };
    match draft.start(context) {
        Ok(started) => Ok(Some(started)),
        Err(error) if execution_deferred(&error) => Err(error),
        Err(error) => {
            store_submit_failure(&draft, &error, context)?;
            Ok(None)
        }
    }
}

fn consume(
    pending: &mut PendingArtifactJob,
    context: &mut PluginContext<'_>,
) -> Result<bool, PluginError> {
    if pending.proposal.is_some() {
        return publish_generated(pending, context);
    }
    let Some(job_id) = pending.job_id.as_ref() else {
        return start_pending(pending, context);
    };
    let output = match context.host_call(CAPABILITY, json!({"operation":"status", "job_id":job_id}))
    {
        Ok(output) => output,
        Err(error) if error.code == "host_capability_unavailable" => {
            return resubmit(pending, context);
        }
        Err(error) => return Err(error),
    };
    let job: ProjectExecutionJob =
        serde_json::from_value(output.clone()).map_err(|error| invalid_status(&output, error))?;
    match job_disposition(&job.status)? {
        JobDisposition::Pending => Ok(false),
        JobDisposition::TerminalFailure => {
            store_failure(pending, &job, context)?;
            Ok(false)
        }
        JobDisposition::Succeeded => complete_generation(pending, job.output, context),
    }
}

fn complete_generation(
    pending: &mut PendingArtifactJob,
    output: Option<Value>,
    context: &mut PluginContext<'_>,
) -> Result<bool, PluginError> {
    let generated = decode_generated(output.as_ref().unwrap_or(&Value::Null), pending)?;
    if !pending_source_selected(pending, context)? {
        pending.finish(None, context)?;
        return Ok(false);
    }
    let document = prepare_generated(pending, generated, context)?;
    pending.freeze_proposal(document, context)?;
    publish_generated(pending, context)
}

fn execution_deferred(error: &PluginError) -> bool {
    error.code == "host_capability_unavailable" || error.code == QUOTA_EXCEEDED
}

fn start_pending(
    pending: &mut PendingArtifactJob,
    context: &mut PluginContext<'_>,
) -> Result<bool, PluginError> {
    match pending.start(context) {
        Ok(started) => *pending = started,
        Err(error) if execution_deferred(&error) => {}
        Err(error) => store_submit_failure(pending, &error, context)?,
    }
    Ok(false)
}

fn job_disposition(status: &str) -> Result<JobDisposition, PluginError> {
    match status {
        "queued" | "running" => Ok(JobDisposition::Pending),
        "succeeded" => Ok(JobDisposition::Succeeded),
        "failed" | "cancelled" => Ok(JobDisposition::TerminalFailure),
        value => Err(PluginError::new(
            "invalid_project_execution_output",
            format!(
                "invalid project execution status `{value}`; expected queued, running, succeeded, failed, or cancelled"
            ),
            false,
        )),
    }
}

fn resubmit(
    pending: &mut PendingArtifactJob,
    context: &mut PluginContext<'_>,
) -> Result<bool, PluginError> {
    if !pending_source_selected(pending, context)? {
        pending.finish(None, context)?;
        return Ok(false);
    }
    let ids = std::collections::HashSet::from([pending.semantic_element_id.clone()]);
    let current = storage::elements_by_ids(context, &pending.project_root, &ids)?;
    if !current
        .iter()
        .any(|element| element.lifecycle == "active" && current_target(element, pending))
    {
        pending.finish(None, context)?;
        return Ok(false);
    }
    if pending.lost_resubmits >= MAX_LOST_RESUBMITS {
        return finish_lost(pending, context);
    }
    let mut retry = pending.clone();
    retry.job_id = None;
    retry.lost_resubmits += 1;
    retry.replace(Some(pending), context)?;
    *pending = retry;
    start_pending(pending, context)
}

fn pending_source_selected(
    pending: &PendingArtifactJob,
    context: &mut PluginContext<'_>,
) -> Result<bool, PluginError> {
    let sources = crate::source_revisions::SelectedSources::load(
        context,
        &pending.project_root,
        std::collections::BTreeSet::from([pending.semantic_element_id.clone()]),
        None,
    )?;
    Ok(sources.matches(&pending.semantic_element_id, &pending.semantic_element_id))
}

fn finish_lost(
    pending: &PendingArtifactJob,
    context: &mut PluginContext<'_>,
) -> Result<bool, PluginError> {
    let failure = ArtifactGenerationFailure::for_pending(
        pending,
        "generation_lost".into(),
        format!(
            "artifact generation job was lost {} times; retry to resubmit",
            pending.lost_resubmits + 1
        ),
    );
    pending.finish(Some(failure), context)?;
    Ok(false)
}

fn prepare_generated(
    pending: &PendingArtifactJob,
    generated: GeneratedFunctionalArtifacts,
    context: &mut PluginContext<'_>,
) -> Result<KnowledgeDocument, PluginError> {
    pending.validate_lineage(context)?;
    let subgraph =
        storage::selective_subgraph(context, &pending.project_root, &pending.semantic_element_id)?
            .ok_or_else(|| stale_target(pending))?;
    let element = subgraph
        .elements
        .iter()
        .find(|element| element.lifecycle == "active" && current_target(element, pending))
        .ok_or_else(|| stale_target(pending))?;
    let summary = FunctionalSummary {
        job: generated.job,
        source_interface: generated.source_interface,
        receives: generated.receives,
        outcome: generated.outcome,
        effects: generated.effects,
    };
    let mut document = functional::generated_document(element, &subgraph.relationships, &summary);
    document.document_id = pending.document.document_id.clone();
    Ok(document)
}

fn publish_generated(
    pending: &PendingArtifactJob,
    context: &mut PluginContext<'_>,
) -> Result<bool, PluginError> {
    pending.validate_lineage(context)?;
    let document = pending
        .proposal
        .as_ref()
        .ok_or_else(|| invalid_job(&json!(pending)))?;
    let saved = documents::save_generated(
        context,
        &documents::DocumentGenerator::FunctionalContext.owner(&pending.semantic_element_id),
        document.clone(),
        pending.document.expected.clone(),
    );
    match saved {
        Ok(_) => Ok(true),
        Err(error) if documents::is_generation_hold(&error) => {
            store_write_hold(pending, error, context)?;
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn store_write_hold(
    pending: &PendingArtifactJob,
    error: PluginError,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let failure = ArtifactGenerationFailure::for_pending(pending, error.code, error.message);
    pending.finish(Some(failure), context)
}

fn decode_generated(
    value: &Value,
    pending: &PendingArtifactJob,
) -> Result<GeneratedFunctionalArtifacts, PluginError> {
    let generated: GeneratedFunctionalArtifacts = serde_json::from_value(value.clone()).map_err(|error| {
        PluginError::new("invalid_project_execution_output",
            format!("invalid functional artifact output `{value}`; expected strict generated artifact: {error}"), false)
    })?;
    if generated.semantic_element_id == pending.semantic_element_id
        && generated.content_fingerprint == pending.content_fingerprint
    {
        return Ok(generated);
    }
    Err(stale_target(pending))
}

fn needs_generation(element: &SemanticElement, artifacts: &[KnowledgeDocument]) -> bool {
    element.lifecycle == "active"
        && target_input(element).is_some()
        && !artifacts.iter().any(|artifact| {
            functional::source_element_id(artifact) == Some(element.semantic_element_id.as_str())
                && artifact.project_root == element.project_root
                && functional::is_usable(artifact)
                && artifact.metadata["provenance"]["content_fingerprint"]
                    == json!(element.content_fingerprint)
        })
}

fn pending_matches(element: &SemanticElement, pending: &[PendingArtifactJob]) -> bool {
    pending.iter().any(|job| current_target(element, job))
}
fn target_input(element: &SemanticElement) -> Option<Value> {
    let fingerprint = element.content_fingerprint.as_deref()?.trim();
    let start_line = u64::try_from(element.start_line?)
        .ok()
        .filter(|line| *line > 0)?;
    let end_line = u64::try_from(element.end_line?)
        .ok()
        .filter(|line| *line >= start_line)?;
    (!fingerprint.is_empty() && !binary_source(&element.path)).then(|| {
        json!({
            "document_id": crate::functional::document_id_for(element),
            "semantic_element_id": element.semantic_element_id,
            "element_kind": element.element_kind,
            "name": element.name,
            "path": element.path,
            "start_line": start_line,
            "end_line": end_line,
            "content_fingerprint": fingerprint,
            "requested_artifact_kinds": ["job", "receives", "outcome", "effects"]
        })
    })
}

/// Binary assets have no source text to summarize; project execution rejects
/// their bytes (`stream did not contain valid UTF-8`), so they are never
/// functional-artifact targets.
fn binary_source(path: &str) -> bool {
    const BINARY_SOURCE_EXTENSIONS: &[&str] = &[
        "png", "ico", "icns", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "mp4", "mov",
        "mp3", "wav", "aiff", "m4a", "ogg", "webm", "zip", "gz", "tgz", "bz2", "xz", "7z", "rar",
        "tar", "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "dylib", "so", "dll", "exe",
        "a", "o", "rlib", "ttf", "otf", "woff", "woff2", "eot", "db", "sqlite", "sqlite3", "onnx",
        "bin", "wasm", "pyc",
    ];
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str());
    extension.is_some_and(|extension| {
        BINARY_SOURCE_EXTENSIONS
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
    })
}
fn skip_submission(
    element: &SemanticElement,
    artifacts: &[KnowledgeDocument],
    pending: &[PendingArtifactJob],
    failures: &[ArtifactGenerationFailure],
    retry_failed: bool,
) -> bool {
    !needs_generation(element, artifacts)
        || pending
            .iter()
            .any(|job| job.job_id.is_some() && current_target(element, job))
        || (!retry_failed
            && failures
                .iter()
                .any(|failure| failure_matches(failure, element)))
}

fn matching_element_ids(
    elements: &[SemanticElement],
    pending: &[PendingArtifactJob],
) -> Vec<String> {
    elements
        .iter()
        .filter(|element| pending_matches(element, pending))
        .map(|element| element.semantic_element_id.clone())
        .collect()
}

fn failure_matches(failure: &ArtifactGenerationFailure, element: &SemanticElement) -> bool {
    failure.project_root == element.project_root
        && failure.semantic_element_id == element.semantic_element_id
        && Some(failure.content_fingerprint.as_str()) == element.content_fingerprint.as_deref()
}

fn failure_matches_elements(
    failure: &ArtifactGenerationFailure,
    elements: &[SemanticElement],
) -> bool {
    elements
        .iter()
        .any(|element| failure_matches(failure, element))
}

fn store_failure(
    pending: &PendingArtifactJob,
    job: &ProjectExecutionJob,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let reason = job.error.clone().unwrap_or_else(|| {
        format!(
            "artifact generation job `{}` ended as `{}`",
            pending.job_id.as_deref().unwrap_or("unsubmitted"),
            job.status
        )
    });
    let failure = ArtifactGenerationFailure::for_pending(
        pending,
        format!("generation_{}", job.status),
        reason,
    );
    pending.finish(Some(failure), context)
}

fn store_submit_failure(
    pending: &PendingArtifactJob,
    error: &PluginError,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let failure = ArtifactGenerationFailure::for_pending(
        pending,
        format!("submit_{}", error.code),
        error.message.clone(),
    );
    pending.finish(Some(failure), context)
}

fn current_target(element: &SemanticElement, pending: &PendingArtifactJob) -> bool {
    element.project_root == pending.project_root
        && element.semantic_element_id == pending.semantic_element_id
        && element.content_fingerprint.as_deref() == Some(pending.content_fingerprint.as_str())
}

#[cfg(test)]
fn pending_key(pending: &PendingArtifactJob) -> String {
    pending.key()
}

fn invalid_job(output: &Value) -> PluginError {
    PluginError::new(
        "invalid_project_execution_output",
        format!("invalid project execution submit output `{output}`; expected job_id"),
        false,
    )
}

fn invalid_status(output: &Value, error: serde_json::Error) -> PluginError {
    PluginError::new(
        "invalid_project_execution_output",
        format!("invalid project execution status `{output}`; expected status and output: {error}"),
        false,
    )
}

fn stale_target(pending: &PendingArtifactJob) -> PluginError {
    PluginError::new(
        "stale_project_execution_output",
        format!(
            "generated target `{}` fingerprint `{}` is stale; expected current semantic element",
            pending.semantic_element_id, pending.content_fingerprint
        ),
        false,
    )
}

#[cfg(test)]
mod consumption_tests;

#[cfg(test)]
mod tests;
