//! Rebuild control records contain identifiers and fingerprints, never source or vectors.
use super::*;
use crate::text_vectors::EmbeddingIdentity;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct Target {
    pub id: String,
    pub path: String,
    pub kind: String,
    pub name_fingerprint: String,
    pub content_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct GenerationCursor {
    pub generation_id: String,
    pub plan_fingerprint: String,
    pub dimensions: usize,
    pub chunk_count: u32,
    pub staged_chunks: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct RebuildRun {
    pub project_root: String,
    pub project_id: String,
    pub run_id: String,
    pub start_digest: String,
    pub revision: u64,
    pub mode: RebuildMode,
    pub paths: Option<Vec<String>>,
    pub status: RebuildStatus,
    pub phase: RebuildPhase,
    pub progress: RebuildProgress,
    pub observed_source_revision: i64,
    pub manifest_fingerprint: String,
    #[serde(skip)]
    pub targets: Vec<Target>,
    pub position: usize,
    pub identity: Option<EmbeddingIdentity>,
    pub generation: Option<GenerationCursor>,
    pub failures: Vec<RebuildFailure>,
}

pub(super) fn targets(elements: &[SemanticElement], paths: Option<&[String]>) -> Vec<Target> {
    let mut targets = elements
        .iter()
        .filter(|element| element.lifecycle == "active")
        .filter(|element| paths.is_none_or(|paths| paths.contains(&element.path)))
        .map(|element| Target {
            id: element.semantic_element_id.clone(),
            path: element.path.clone(),
            kind: element.element_kind.clone(),
            name_fingerprint: hash(element.name.trim().as_bytes()),
            content_fingerprint: element.content_fingerprint.clone(),
        })
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| (&left.path, &left.id).cmp(&(&right.path, &right.id)));
    targets
}

pub(super) fn run_key(root: &str, project_id: &str, id: &str) -> String {
    storage::project_key(root, "vector-rebuild", &[project_id, id])
}

pub(super) fn active_key(root: &str, project_id: &str) -> String {
    storage::project_key(root, "vector-rebuild-active", &[project_id])
}

pub(super) fn load(
    context: &mut PluginContext<'_>,
    root: &str,
    project_id: &str,
    id: &str,
) -> Result<RebuildRun, PluginError> {
    let mut run = load_header(context, root, project_id, id)?;
    let key = manifest_key(root, project_id, &run.manifest_fingerprint);
    run.targets = storage::get(context, storage::SEARCH_INDEX_STATE, &key)?.ok_or_else(|| {
        failure(
            "invalid_rebuild_manifest",
            "expected immutable rebuild target manifest",
        )
    })?;
    verify_manifest(&run)?;
    Ok(run)
}

fn load_header(
    context: &mut PluginContext<'_>,
    root: &str,
    project_id: &str,
    id: &str,
) -> Result<RebuildRun, PluginError> {
    let run: RebuildRun = storage::get(
        context,
        storage::SEARCH_INDEX_STATE,
        &run_key(root, project_id, id),
    )?
    .ok_or_else(|| {
        failure(
            "unknown_rebuild_run",
            "expected an existing run in this project",
        )
    })?;
    if run.project_root != root || run.project_id != project_id || run.run_id != id {
        return Err(failure(
            "rebuild_scope_conflict",
            "stored run does not match requested project and run id",
        ));
    }
    Ok(run)
}

fn verify_manifest(run: &RebuildRun) -> Result<(), PluginError> {
    if digest(&run.targets)? == run.manifest_fingerprint {
        return Ok(());
    }
    Err(failure(
        "invalid_rebuild_manifest",
        "target manifest must match its project-scoped content address",
    ))
}

fn manifest_key(root: &str, project_id: &str, fingerprint: &str) -> String {
    storage::project_key(root, "vector-rebuild-manifest", &[project_id, fingerprint])
}

pub(super) fn create(
    context: &mut PluginContext<'_>,
    run: &RebuildRun,
    active: Option<&Value>,
) -> Result<(), PluginError> {
    verify_manifest(run)?;
    let manifest = manifest_key(
        &run.project_root,
        &run.project_id,
        &run.manifest_fingerprint,
    );
    storage::put(
        context,
        storage::SEARCH_INDEX_STATE,
        &manifest,
        &run.targets,
    )?;
    let changes = creation_changes(run, active)?;
    publish_control_changes(context, changes, run)
}

fn creation_changes(
    run: &RebuildRun,
    active: Option<&Value>,
) -> Result<Vec<storage::Mutation>, PluginError> {
    let key = run_key(&run.project_root, &run.project_id, &run.run_id);
    let active_key = active_key(&run.project_root, &run.project_id);
    let table = storage::SEARCH_INDEX_STATE;
    Ok(vec![
        storage::require_row_mutation(table, &key, None),
        storage::require_row_mutation(table, &active_key, active),
        storage::put_mutation(table, &key, run)?,
        storage::put_mutation(table, &active_key, &json!({"run_id":run.run_id}))?,
    ])
}

pub(super) fn save(
    context: &mut PluginContext<'_>,
    previous: &RebuildRun,
    next: &mut RebuildRun,
) -> Result<(), PluginError> {
    next.revision = previous.revision.checked_add(1).ok_or_else(|| {
        failure(
            "rebuild_revision_overflow",
            "expected an incrementable revision",
        )
    })?;
    let changes = cursor_changes(previous, next)?;
    publish_control_changes(context, changes, next)
}

fn cursor_changes(
    previous: &RebuildRun,
    next: &RebuildRun,
) -> Result<Vec<storage::Mutation>, PluginError> {
    let key = run_key(
        &previous.project_root,
        &previous.project_id,
        &previous.run_id,
    );
    let table = storage::SEARCH_INDEX_STATE;
    let expected = control_value(previous)?;
    Ok(vec![
        storage::require_row_mutation(table, &key, Some(&expected)),
        storage::require_row_mutation(
            table,
            &active_key(&previous.project_root, &previous.project_id),
            Some(&json!({"run_id":previous.run_id})),
        ),
        storage::put_mutation(table, &key, next)?,
    ])
}

fn control_value(run: &RebuildRun) -> Result<Value, PluginError> {
    serde_json::to_value(run).map_err(|_| {
        failure(
            "invalid_rebuild_record",
            "expected serializable control metadata",
        )
    })
}

fn publish_control_changes(
    context: &mut PluginContext<'_>,
    mut changes: Vec<storage::Mutation>,
    run: &RebuildRun,
) -> Result<(), PluginError> {
    add_readiness(&mut changes, run)?;
    storage::apply_mutations(context, &changes).map_err(|_| failure("rebuild_state_conflict",
        "run cursor or active project run changed, or acknowledgement was lost; reread before retry"))
}

fn add_readiness(
    changes: &mut Vec<storage::Mutation>,
    run: &RebuildRun,
) -> Result<(), PluginError> {
    if run.mode == RebuildMode::Text {
        return Ok(());
    }
    let state = match run.status {
        RebuildStatus::Complete => "ready",
        RebuildStatus::Pending => "rebuilding",
        _ => "failed",
    };
    let key = storage::project_key(&run.project_root, "search-index", &["state"]);
    changes.push(storage::put_mutation(storage::SEARCH_INDEX_STATE, &key, &json!({
        "project_root":run.project_root, "state":state, "indexed_nodes":run.progress.names_completed,
        "error":null, "observed_source_revision":run.observed_source_revision,
    }))?);
    Ok(())
}

pub(super) fn response(
    context: &mut PluginContext<'_>,
    run: RebuildRun,
) -> Result<Value, PluginError> {
    let index_state = read_index_state(context, &run.project_root)?;
    serialize_response(response_record(run, index_state))
}

fn read_index_state(context: &mut PluginContext<'_>, root: &str) -> Result<String, PluginError> {
    let key = storage::project_key(root, "search-index", &["state"]);
    let header = storage::get::<Value>(context, storage::SEARCH_INDEX_STATE, &key)?;
    Ok(header
        .as_ref()
        .and_then(|value| value["state"].as_str())
        .unwrap_or("missing")
        .to_owned())
}

fn response_record(run: RebuildRun, index_state: String) -> RebuildSearchIndexResponse {
    let continuation = (run.status == RebuildStatus::Pending).then(|| RebuildContinuation {
        run_id: run.run_id.clone(),
        expected_revision: run.revision,
    });
    RebuildSearchIndexResponse {
        project_root: run.project_root,
        run_id: run.run_id,
        revision: run.revision,
        status: run.status,
        phase: run.phase,
        progress: run.progress,
        continuation,
        index_state,
        observed_source_revision: run.observed_source_revision,
        failures: run.failures,
    }
}
