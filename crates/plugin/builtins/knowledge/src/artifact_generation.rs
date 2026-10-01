use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    KnowledgeArtifact, cultivation,
    functional::{self, FunctionalSummary},
    semantic_context::SemanticElement,
    storage,
};

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

#[derive(Clone, Deserialize, Serialize)]
struct PendingArtifactJob {
    job_id: String,
    project_root: String,
    semantic_element_id: String,
    content_fingerprint: String,
    #[serde(default)]
    priority: SubmitPriority,
    #[serde(default)]
    lost_resubmits: u32,
}

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
    artifacts: &[KnowledgeArtifact],
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
    let pending = storage::list::<PendingArtifactJob>(context, JOBS, None)?;
    let failures = storage::list::<ArtifactGenerationFailure>(context, FAILURES, None)?;
    let mut submitted = 0;
    for element in elements {
        if skip_submission(element, artifacts, &pending, &failures, retry_failed) {
            continue;
        }
        if retry_failed {
            storage::delete(context, FAILURES, &job_key(element))?;
        }
        match submit(element, priority, context) {
            Ok(Some(job)) => {
                storage::put(context, JOBS, &job_key(element), &job)?;
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
            Err(error) => {
                // One invalid target (for example a binary source file that
                // project execution refuses to read) must not poison the
                // batch: record it per element so the remaining elements
                // still submit and later polls skip this one instead of
                // dead-lettering every delivery.
                store_submit_failure(element, &error, context)?;
            }
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

    let pending = storage::list::<PendingArtifactJob>(context, JOBS, None)?;
    let mut completed_by_project = BTreeMap::<String, Vec<PendingArtifactJob>>::new();
    let mut consumption_error = None;
    for job in pending
        .into_iter()
        .filter(|job| project_root.is_none_or(|root| job.project_root == root))
    {
        match consume(&job, context) {
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
            storage::delete(context, JOBS, &pending_key(&job))?;
            completed += 1;
        }
    }
    if let Some(error) = consumption_error {
        return Err(error);
    }
    Ok(
        json!({"pending": storage::list::<PendingArtifactJob>(context, JOBS, None)?.len(),
        "completed": completed}),
    )
}

pub(crate) fn state_for(
    elements: &[SemanticElement],
    context: &mut PluginContext<'_>,
) -> Result<ArtifactGenerationState, PluginError> {
    let pending = storage::list::<PendingArtifactJob>(context, JOBS, None)?;
    let failures = storage::list::<ArtifactGenerationFailure>(context, FAILURES, None)?;
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
    artifacts: &[KnowledgeArtifact],
) -> Vec<String> {
    elements
        .iter()
        .filter(|element| needs_generation(element, artifacts))
        .map(|element| element.semantic_element_id.clone())
        .collect()
}

fn submit(
    element: &SemanticElement,
    priority: SubmitPriority,
    context: &mut PluginContext<'_>,
) -> Result<Option<PendingArtifactJob>, PluginError> {
    let Some(target) = target_input(element) else {
        return Ok(None);
    };
    let output = context.host_call(
        CAPABILITY,
        json!({"operation": "submit", "project_root": element.project_root,
            "capability_id": GENERATOR,
            "idempotency_key": format!("functional:{}:{}:{}", element.project_root,
                element.semantic_element_id,
                element.content_fingerprint.as_deref().unwrap_or_default()),
            "input": target, "priority": priority}),
    )?;
    let job_id = output["job_id"]
        .as_str()
        .ok_or_else(|| invalid_job(&output))?;
    Ok(Some(PendingArtifactJob {
        job_id: job_id.into(),
        project_root: element.project_root.clone(),
        semantic_element_id: element.semantic_element_id.clone(),
        content_fingerprint: element.content_fingerprint.clone().unwrap_or_default(),
        priority,
        lost_resubmits: 0,
    }))
}

fn consume(
    pending: &PendingArtifactJob,
    context: &mut PluginContext<'_>,
) -> Result<bool, PluginError> {
    let output = match context.host_call(
        CAPABILITY,
        json!({"operation": "status", "job_id": pending.job_id}),
    ) {
        Ok(output) => output,
        Err(error) if error.code == "host_capability_unavailable" => {
            return resubmit(pending, context);
        }
        Err(error) => return Err(error),
    };
    let job: ProjectExecutionJob =
        serde_json::from_value(output.clone()).map_err(|error| invalid_status(&output, error))?;
    match job_disposition(&job.status)? {
        JobDisposition::Pending => return Ok(false),
        JobDisposition::TerminalFailure => {
            store_failure(pending, &job, context)?;
            storage::delete(context, JOBS, &pending_key(pending))?;
            return Ok(false);
        }
        JobDisposition::Succeeded => {}
    }
    let generated = decode_generated(job.output.as_ref().unwrap_or(&Value::Null), pending)?;
    store_generated(pending, generated, context)?;
    Ok(true)
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
    pending: &PendingArtifactJob,
    context: &mut PluginContext<'_>,
) -> Result<bool, PluginError> {
    // A point read: after a restart every stale job lands here, and a full
    // project snapshot per job overran the invocation deadline.
    let ids = std::collections::HashSet::from([pending.semantic_element_id.clone()]);
    let current = storage::elements_by_ids(context, &pending.project_root, &ids)?
        .into_iter()
        .find(|element| {
            element.semantic_element_id == pending.semantic_element_id
                && element.lifecycle == "active"
        });
    let Some(element) = current else {
        storage::delete(context, JOBS, &pending_key(pending))?;
        return Ok(false);
    };
    let lost_resubmits = pending.lost_resubmits + 1;
    if lost_resubmits > MAX_LOST_RESUBMITS {
        let failure = ArtifactGenerationFailure {
            project_root: pending.project_root.clone(),
            semantic_element_id: pending.semantic_element_id.clone(),
            content_fingerprint: pending.content_fingerprint.clone(),
            reason_code: "generation_lost".into(),
            reason: format!(
                "artifact generation job was lost {lost_resubmits} times; retry to resubmit"
            ),
        };
        storage::put(context, FAILURES, &pending_key(pending), &failure)?;
        storage::delete(context, JOBS, &pending_key(pending))?;
        return Ok(false);
    }
    match submit(&element, pending.priority, context) {
        Ok(Some(job)) => {
            let job = PendingArtifactJob {
                lost_resubmits,
                ..job
            };
            storage::put(context, JOBS, &pending_key(pending), &job)?
        }
        Ok(None) => storage::delete(context, JOBS, &pending_key(pending))?,
        Err(error)
            if error.code == "host_capability_unavailable" || error.code == QUOTA_EXCEEDED =>
        {
            return Ok(false);
        }
        Err(error) => {
            // The stale job must go too, or every poll resubmits and fails again.
            store_submit_failure(&element, &error, context)?;
            storage::delete(context, JOBS, &pending_key(pending))?;
        }
    }
    Ok(false)
}

fn store_generated(
    pending: &PendingArtifactJob,
    generated: GeneratedFunctionalArtifacts,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
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
    let artifact = functional::generated_artifact(element, &subgraph.relationships, &summary);
    storage::put_knowledge(context, &artifact)?;
    storage::delete(context, FAILURES, &pending_key(pending))?;
    Ok(())
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

fn needs_generation(element: &SemanticElement, artifacts: &[KnowledgeArtifact]) -> bool {
    element.lifecycle == "active"
        && target_input(element).is_some()
        && !artifacts.iter().any(|artifact| {
            artifact.semantic_element_id == element.semantic_element_id
                && functional::is_usable(artifact)
                && artifact.metadata["provenance"]["content_fingerprint"]
                    == json!(element.content_fingerprint)
        })
}

fn pending_matches(element: &SemanticElement, pending: &[PendingArtifactJob]) -> bool {
    pending.iter().any(|job| {
        job.project_root == element.project_root
            && job.semantic_element_id == element.semantic_element_id
            && Some(job.content_fingerprint.as_str()) == element.content_fingerprint.as_deref()
    })
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
            "artifact_id": crate::functional::artifact_id_for(element),
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
    artifacts: &[KnowledgeArtifact],
    pending: &[PendingArtifactJob],
    failures: &[ArtifactGenerationFailure],
    retry_failed: bool,
) -> bool {
    !needs_generation(element, artifacts)
        || pending_matches(element, pending)
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
    let failure = ArtifactGenerationFailure {
        project_root: pending.project_root.clone(),
        semantic_element_id: pending.semantic_element_id.clone(),
        content_fingerprint: pending.content_fingerprint.clone(),
        reason_code: format!("generation_{}", job.status),
        reason: job.error.clone().unwrap_or_else(|| {
            format!(
                "artifact generation job `{}` ended as `{}`",
                pending.job_id, job.status
            )
        }),
    };
    storage::put(context, FAILURES, &pending_key(pending), &failure)
}

fn store_submit_failure(
    element: &SemanticElement,
    error: &PluginError,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let failure = ArtifactGenerationFailure {
        project_root: element.project_root.clone(),
        semantic_element_id: element.semantic_element_id.clone(),
        content_fingerprint: element.content_fingerprint.clone().unwrap_or_default(),
        reason_code: format!("submit_{}", error.code),
        reason: error.message.clone(),
    };
    storage::put(context, FAILURES, &job_key(element), &failure)
}

fn current_target(element: &SemanticElement, pending: &PendingArtifactJob) -> bool {
    element.semantic_element_id == pending.semantic_element_id
        && element.content_fingerprint.as_deref() == Some(pending.content_fingerprint.as_str())
}

fn job_key(element: &SemanticElement) -> String {
    format!("{}\0{}", element.project_root, element.semantic_element_id)
}

fn pending_key(pending: &PendingArtifactJob) -> String {
    format!("{}\0{}", pending.project_root, pending.semantic_element_id)
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
