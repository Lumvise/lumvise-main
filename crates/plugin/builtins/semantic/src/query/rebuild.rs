//! Owns resumable rebuild execution; persistence contracts retain storage/publication ownership.
use crate::models::{RebuildMode, RebuildSearchIndexRequest};
mod section;
mod state;
#[cfg(test)]
mod tests;
mod work;

use crate::rebuild_contract::{
    RebuildContinuation, RebuildFailure, RebuildPhase, RebuildProgress, RebuildSearchIndexResponse,
    RebuildStatus,
};
use crate::{models::SemanticElement, parse, require_non_empty, serialize_response, storage};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use state::RebuildRun;
use std::time::{Duration, Instant};

pub(super) fn invoke(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let started = Instant::now();
    invoke_with_budget(input, context, || {
        started.elapsed() < Duration::from_secs(30)
    })
}

fn invoke_with_budget(
    input: Value,
    context: &mut PluginContext<'_>,
    mut may_start: impl FnMut() -> bool,
) -> Result<Value, PluginError> {
    let request: RebuildSearchIndexRequest = parse(input, "tagged rebuild request")?;
    let project_id = authorize_request(context, &request)?;
    let run = dispatch_request(context, request, &project_id, &mut may_start)?;
    state::response(context, run)
}

fn authorize_request(
    context: &mut PluginContext<'_>,
    request: &RebuildSearchIndexRequest,
) -> Result<String, PluginError> {
    let root = request_root(request);
    require_non_empty(root, "project_root")?;
    // Every action, including replay/terminal reads, crosses the scoped source authority.
    work::source_snapshot(context, root)?;
    read_project_id(context, root)
}

fn read_project_id(context: &mut PluginContext<'_>, root: &str) -> Result<String, PluginError> {
    let output = context.host_call(
        "storage.semantic",
        json!({"operation":"project_identity","project_root":root}),
    )?;
    output["identity"]["project_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            failure(
                "invalid_rebuild_project_identity",
                "expected current authorized durable project UUID",
            )
        })
}

fn request_root(request: &RebuildSearchIndexRequest) -> &str {
    match request {
        RebuildSearchIndexRequest::Start { project_root, .. }
        | RebuildSearchIndexRequest::Continue { project_root, .. }
        | RebuildSearchIndexRequest::Cancel { project_root, .. } => project_root,
    }
}

fn dispatch_request(
    context: &mut PluginContext<'_>,
    request: RebuildSearchIndexRequest,
    project_id: &str,
    budget: &mut impl FnMut() -> bool,
) -> Result<RebuildRun, PluginError> {
    use RebuildSearchIndexRequest::{Cancel, Continue, Start};
    match request {
        Start {
            project_root,
            operation_id,
            mode,
            paths,
        } => start(
            context,
            project_root,
            project_id,
            operation_id,
            mode,
            paths,
            budget,
        ),
        Continue {
            project_root,
            run_id,
            expected_revision,
        } => continue_run(
            context,
            &project_root,
            project_id,
            &run_id,
            expected_revision,
            budget,
        ),
        Cancel {
            project_root,
            run_id,
            expected_revision,
        } => cancel_request(
            context,
            &project_root,
            project_id,
            &run_id,
            expected_revision,
        ),
    }
}

fn continue_run(
    context: &mut PluginContext<'_>,
    root: &str,
    project_id: &str,
    id: &str,
    revision: u64,
    budget: &mut impl FnMut() -> bool,
) -> Result<RebuildRun, PluginError> {
    let run = checked_run(context, root, project_id, id, revision)?;
    work::drive(context, run, budget)
}

fn cancel_request(
    context: &mut PluginContext<'_>,
    root: &str,
    project_id: &str,
    id: &str,
    revision: u64,
) -> Result<RebuildRun, PluginError> {
    let run = state::load(context, root, project_id, id)?;
    let replay =
        run.status == RebuildStatus::Canceled && revision.checked_add(1) == Some(run.revision);
    if !replay && revision != run.revision {
        return Err(failure(
            "rebuild_revision_conflict",
            &format!(
                "received cancel expected_revision {revision}; expected current stored revision {}",
                run.revision
            ),
        ));
    }
    cancel(context, run)
}

fn start(
    context: &mut PluginContext<'_>,
    root: String,
    project_id: &str,
    operation: String,
    mode: RebuildMode,
    paths: Option<Vec<String>>,
    budget: &mut impl FnMut() -> bool,
) -> Result<RebuildRun, PluginError> {
    validate_start(&root, &operation, mode, paths.as_deref())?;
    storage::ensure(context, storage::SEARCH_INDEX_STATE)?;
    let id = digest(&json!([root, project_id, operation]))?;
    let start_digest = digest(&json!([root, project_id, operation, mode, paths]))?;
    if let Some(existing) = replay_start(context, &root, project_id, &id, &start_digest)? {
        return Ok(existing);
    }
    let run = initialize_run(context, root, project_id, id, start_digest, mode, paths)?;
    work::drive(context, run, budget)
}

fn validate_start(
    root: &str,
    operation: &str,
    mode: RebuildMode,
    paths: Option<&[String]>,
) -> Result<(), PluginError> {
    require_non_empty(root, "project_root")?;
    require_non_empty(operation, "operation_id")?;
    if paths.is_some() && mode != RebuildMode::Text {
        return Err(failure(
            "invalid_semantic_input",
            &format!("received paths with mode {mode:?}; expected text start"),
        ));
    }
    Ok(())
}

fn replay_start(
    context: &mut PluginContext<'_>,
    root: &str,
    project_id: &str,
    id: &str,
    start_digest: &str,
) -> Result<Option<RebuildRun>, PluginError> {
    let existing = storage::get::<RebuildRun>(
        context,
        storage::SEARCH_INDEX_STATE,
        &state::run_key(root, project_id, id),
    )?;
    let Some(existing) = existing else {
        return Ok(None);
    };
    verify_start_digest(&existing, start_digest)?;
    state::load(context, root, project_id, id).map(Some)
}

fn verify_start_digest(existing: &RebuildRun, expected: &str) -> Result<(), PluginError> {
    if existing.start_digest == expected {
        return Ok(());
    }
    Err(failure(
        "rebuild_operation_conflict",
        "operation_id was used with a different start request",
    ))
}

fn initialize_run(
    context: &mut PluginContext<'_>,
    root: String,
    project_id: &str,
    id: String,
    start_digest: String,
    mode: RebuildMode,
    paths: Option<Vec<String>>,
) -> Result<RebuildRun, PluginError> {
    let active = require_idle(context, &root, project_id)?;
    let snapshot = work::source_snapshot(context, &root)?;
    let targets = state::targets(&snapshot.elements, paths.as_deref());
    let identity = work::initial_identity(context, &snapshot.elements, &targets)?;
    let run = initial_record(
        root,
        project_id.into(),
        id,
        start_digest,
        mode,
        paths,
        snapshot.commit_version,
        targets,
        identity,
    )?;
    state::create(context, &run, active.as_ref())?;
    Ok(run)
}

fn initial_record(
    root: String,
    project_id: String,
    id: String,
    start_digest: String,
    mode: RebuildMode,
    paths: Option<Vec<String>>,
    source_revision: i64,
    targets: Vec<state::Target>,
    identity: Option<crate::text_vectors::EmbeddingIdentity>,
) -> Result<RebuildRun, PluginError> {
    Ok(RebuildRun {
        project_root: root,
        project_id,
        run_id: id,
        start_digest,
        revision: 1,
        mode,
        paths,
        status: RebuildStatus::Pending,
        phase: if mode == RebuildMode::Text {
            RebuildPhase::Text
        } else {
            RebuildPhase::Names
        },
        progress: RebuildProgress::default(),
        observed_source_revision: source_revision,
        manifest_fingerprint: digest(&targets)?,
        targets,
        position: 0,
        identity,
        generation: None,
        failures: Vec::new(),
    })
}

fn require_idle(
    context: &mut PluginContext<'_>,
    root: &str,
    project_id: &str,
) -> Result<Option<Value>, PluginError> {
    let active = storage::get::<Value>(
        context,
        storage::SEARCH_INDEX_STATE,
        &state::active_key(root, project_id),
    )?;
    if let Some(id) = active.as_ref().and_then(|value| value["run_id"].as_str()) {
        if state::load(context, root, project_id, id)?.status == RebuildStatus::Pending {
            return Err(failure(
                "rebuild_in_progress",
                "cancel or continue the active project rebuild before starting another",
            ));
        }
    }
    Ok(active)
}

fn checked_run(
    context: &mut PluginContext<'_>,
    root: &str,
    project_id: &str,
    id: &str,
    revision: u64,
) -> Result<RebuildRun, PluginError> {
    require_non_empty(root, "project_root")?;
    require_non_empty(id, "run_id")?;
    let run = state::load(context, root, project_id, id)?;
    if run.revision != revision {
        return Err(failure(
            "rebuild_revision_conflict",
            &format!(
                "received expected_revision {revision}; expected current stored revision {}",
                run.revision
            ),
        ));
    }
    Ok(run)
}

fn cancel(context: &mut PluginContext<'_>, mut run: RebuildRun) -> Result<RebuildRun, PluginError> {
    if run.status == RebuildStatus::Canceled {
        section::abort(context, &run)?;
        return Ok(run);
    }
    if run.status != RebuildStatus::Pending {
        return Ok(run);
    }
    let previous = run.clone();
    run.status = RebuildStatus::Canceled;
    run.failures.push(RebuildFailure { element_id: run.targets.get(run.position).map(|target| target.id.clone()), path: None,
        code: "rebuild_canceled".into(), message: "Cancellation is cooperative; already committed or in-flight complete publications are not rolled back.".into() });
    state::save(context, &previous, &mut run)?;
    section::abort(context, &run)?;
    Ok(run)
}

fn digest(value: &impl serde::Serialize) -> Result<String, PluginError> {
    serde_json::to_vec(value)
        .map(|bytes| hash(&bytes))
        .map_err(|_| {
            failure(
                "invalid_rebuild_metadata",
                "expected serializable identifiers and fingerprints",
            )
        })
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn failure(code: &str, message: &str) -> PluginError {
    PluginError::new(code, message, true)
}
