//! Runs one durable domain unit at a time; the clock only decides when to start more work.
use super::state::Target;
use super::*;
use crate::text_vectors::{self, EmbeddingIdentity, PlannedSection, SourcePages};

type SourceSnapshot = storage::ProjectSnapshot<SemanticElement, Value, Value>;

#[derive(Default)]
pub(super) struct TransientWork {
    pub sources: SourcePages,
    pub plan: Option<(String, PlannedSection)>,
    pub first_vector: Option<(String, Vec<f32>)>,
}

impl TransientWork {
    fn cache_plan(
        &mut self,
        context: &mut PluginContext<'_>,
        root: &str,
        element: &SemanticElement,
    ) -> Result<(), PluginError> {
        if self
            .plan
            .as_ref()
            .is_some_and(|(id, _)| id == &element.semantic_element_id)
        {
            return Ok(());
        }
        let plan = self.sources.section(context, root, element)?;
        self.plan = Some((element.semantic_element_id.clone(), plan));
        Ok(())
    }

    fn cached_plan(&self) -> Result<&PlannedSection, PluginError> {
        self.plan
            .as_ref()
            .map(|(_, plan)| plan)
            .ok_or_else(|| failure("rebuild_source_conflict", "expected transient section plan"))
    }
}

pub(super) fn source_snapshot(
    context: &mut PluginContext<'_>,
    root: &str,
) -> Result<SourceSnapshot, PluginError> {
    storage::project_snapshot(context, &json!({"project_root":root}), None)
}

pub(super) fn initial_identity(
    context: &mut PluginContext<'_>,
    elements: &[SemanticElement],
    targets: &[Target],
) -> Result<Option<EmbeddingIdentity>, PluginError> {
    let Some(first) = targets.first() else {
        return Ok(None);
    };
    text_vectors::plan_identity(context, &current_element(elements, &first.id)?.name).map(Some)
}

pub(super) fn drive(
    context: &mut PluginContext<'_>,
    mut run: RebuildRun,
    budget: &mut impl FnMut() -> bool,
) -> Result<RebuildRun, PluginError> {
    if run.status != RebuildStatus::Pending {
        return Ok(run);
    }
    let snapshot = source_snapshot(context, &run.project_root)?;
    if verify_targets(&run, &snapshot).is_err() {
        return terminate(context, run, "rebuild_source_conflict");
    }
    let mut transient = TransientWork::default();
    while run.status == RebuildStatus::Pending && budget() {
        run = checkpoint_unit(context, run, &snapshot.elements, &mut transient)?;
    }
    Ok(run)
}

fn checkpoint_unit(
    context: &mut PluginContext<'_>,
    mut run: RebuildRun,
    elements: &[SemanticElement],
    transient: &mut TransientWork,
) -> Result<RebuildRun, PluginError> {
    let previous = run.clone();
    if let Err(error) = step(context, &mut run, elements, transient) {
        return failed_unit(context, previous, error);
    }
    state::save(context, &previous, &mut run)?;
    Ok(run)
}

fn failed_unit(
    context: &mut PluginContext<'_>,
    run: RebuildRun,
    error: PluginError,
) -> Result<RebuildRun, PluginError> {
    if error.code == "rebuild_state_conflict" {
        return Err(error);
    }
    if error.retryable && safe_failure_code(&error) == "rebuild_step_failed" {
        return Err(failure(
            "rebuild_retry_required",
            "work acknowledgement was not confirmed; replay the start operation or continue the same durable cursor",
        ));
    }
    terminate(context, run, safe_failure_code(&error))
}

fn step(
    context: &mut PluginContext<'_>,
    run: &mut RebuildRun,
    elements: &[SemanticElement],
    transient: &mut TransientWork,
) -> Result<(), PluginError> {
    let Some(target) = run.targets.get(run.position) else {
        verify_completion_source(context, run)?;
        return finish_phase(run);
    };
    let element = current_element(elements, &target.id)?;
    verify_identity(run, &text_vectors::plan_identity(context, &element.name)?)?;
    dispatch_step(context, run, element, transient)
}

fn dispatch_step(
    context: &mut PluginContext<'_>,
    run: &mut RebuildRun,
    element: &SemanticElement,
    transient: &mut TransientWork,
) -> Result<(), PluginError> {
    match run.phase {
        RebuildPhase::Names => name_step(context, run, element),
        RebuildPhase::Text if element.element_kind != "html_section" => {
            run.position += 1;
            Ok(())
        }
        RebuildPhase::Text => section::step(context, run, element, transient),
        RebuildPhase::Done => Err(failure(
            "invalid_rebuild_phase",
            "pending rebuild cannot have done phase",
        )),
    }
}

fn current_element<'a>(
    elements: &'a [SemanticElement],
    id: &str,
) -> Result<&'a SemanticElement, PluginError> {
    elements
        .iter()
        .find(|element| element.semantic_element_id == id)
        .ok_or_else(|| failure("rebuild_source_conflict", "expected current source element"))
}

fn verify_identity(run: &RebuildRun, selected: &EmbeddingIdentity) -> Result<(), PluginError> {
    if run.identity.as_ref() == Some(selected) {
        return Ok(());
    }
    Err(failure(
        "rebuild_profile_conflict",
        "selected embedding profile differs from pinned rebuild identity",
    ))
}

fn verify_targets(run: &RebuildRun, snapshot: &SourceSnapshot) -> Result<(), PluginError> {
    if state::targets(&snapshot.elements, run.paths.as_deref()) == run.targets {
        return Ok(());
    }
    Err(failure(
        "rebuild_source_conflict",
        "source target set changed before completion",
    ))
}

pub(super) fn pinned_identity(run: &RebuildRun) -> Result<&EmbeddingIdentity, PluginError> {
    run.identity.as_ref().ok_or_else(|| {
        failure(
            "rebuild_profile_conflict",
            "expected pinned embedding identity",
        )
    })
}

fn name_step(
    context: &mut PluginContext<'_>,
    run: &mut RebuildRun,
    element: &SemanticElement,
) -> Result<(), PluginError> {
    if !element.name.trim().is_empty() {
        publish_name(context, run, element)?;
    }
    run.progress.names_completed += 1;
    run.position += 1;
    Ok(())
}

fn publish_name(
    context: &mut PluginContext<'_>,
    run: &RebuildRun,
    element: &SemanticElement,
) -> Result<(), PluginError> {
    let identity = pinned_identity(run)?;
    let vector = text_vectors::embed_pinned(context, element.name.trim(), identity)?;
    storage::store_element_name_vectors(
        context,
        &run.project_root,
        vec![json!({
            "semantic_element_id":element.semantic_element_id,"project_root":run.project_root,"source_text":element.name.trim(),
            "vector":{"engine_id":identity.engine_id,"model":identity.model,"dimensions":vector.len(),"vector":vector,"normalized":false,"metadata":{}}
        })],
    )?;
    Ok(())
}

fn finish_phase(run: &mut RebuildRun) -> Result<(), PluginError> {
    if run.phase == RebuildPhase::Names && run.mode == RebuildMode::Full {
        run.phase = RebuildPhase::Text;
        run.position = 0;
        return Ok(());
    }
    run.phase = RebuildPhase::Done;
    run.status = RebuildStatus::Complete;
    Ok(())
}

fn safe_failure_code(error: &PluginError) -> &'static str {
    match error.code.as_str() {
        "rebuild_source_conflict" => "rebuild_source_conflict",
        "rebuild_profile_conflict" | "embedding_profile_conflict" => "rebuild_profile_conflict",
        "rebuild_generation_conflict" => "rebuild_generation_conflict",
        _ => "rebuild_step_failed",
    }
}

pub(super) fn terminate(
    context: &mut PluginContext<'_>,
    mut run: RebuildRun,
    code: &str,
) -> Result<RebuildRun, PluginError> {
    let previous = run.clone();
    section::abort(context, &run)?;
    run.status = if code.ends_with("conflict") {
        RebuildStatus::Conflicted
    } else {
        RebuildStatus::Failed
    };
    run.failures.push(RebuildFailure { element_id: run.targets.get(run.position).map(|target| target.id.clone()),
        path: run.targets.get(run.position).map(|target| target.path.clone()), code: code.into(),
        message: "Rebuild could not finish its current unit; earlier complete publications remain. Source bodies and raw host errors were not saved.".into() });
    state::save(context, &previous, &mut run)?;
    Ok(run)
}

pub(super) fn ensure_plan<'a>(
    context: &mut PluginContext<'_>,
    run: &RebuildRun,
    element: &SemanticElement,
    transient: &'a mut TransientWork,
) -> Result<&'a PlannedSection, PluginError> {
    transient.cache_plan(context, &run.project_root, element)?;
    let plan = transient.cached_plan()?;
    verify_identity(run, &plan.identity)?;
    Ok(plan)
}

fn verify_completion_source(
    context: &mut PluginContext<'_>,
    run: &mut RebuildRun,
) -> Result<(), PluginError> {
    let latest = source_snapshot(context, &run.project_root)?;
    verify_targets(run, &latest)?;
    if let Some(identity) = initial_identity(context, &latest.elements, &run.targets)? {
        verify_identity(run, &identity)?;
    }
    run.observed_source_revision = latest.commit_version;
    Ok(())
}
