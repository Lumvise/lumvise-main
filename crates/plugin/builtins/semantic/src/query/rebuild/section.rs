//! Adapts complete-section generations to DB Core; relational progress stores no vectors.
use super::state::GenerationCursor;
use super::work::{TransientWork, ensure_plan};
use super::*;
use crate::text_vectors;
use serde::Deserialize;

#[derive(Deserialize)]
struct GenerationStatus {
    key: Value,
    state: String,
    next_chunk: u32,
    chunk_count: u32,
    published_count: usize,
}

pub(super) fn step(
    context: &mut PluginContext<'_>,
    run: &mut RebuildRun,
    element: &SemanticElement,
    transient: &mut TransientWork,
) -> Result<(), PluginError> {
    let plan = ensure_plan(context, run, element, transient)?;
    if plan.ranges.is_empty() {
        finish(run);
        return Ok(());
    }
    if run.generation.is_none() {
        return prepare(context, run, element, transient);
    }
    let status = begin_planned_generation(context, run, element, plan)?;
    resume_generation(context, run, element, transient, status)
}

fn begin_planned_generation(
    context: &mut PluginContext<'_>,
    run: &RebuildRun,
    element: &SemanticElement,
    plan: &text_vectors::PlannedSection,
) -> Result<GenerationStatus, PluginError> {
    let cursor = cursor(run)?;
    if digest(&plan.ranges)? != cursor.plan_fingerprint {
        return Err(conflict("source chunk plan changed across continuation"));
    }
    generation_call(
        context,
        "begin_element_text_vector_generation",
        json!({"spec":spec(run, element, cursor)?}),
        run,
    )
}

fn resume_generation(
    context: &mut PluginContext<'_>,
    run: &mut RebuildRun,
    element: &SemanticElement,
    transient: &mut TransientWork,
    status: GenerationStatus,
) -> Result<(), PluginError> {
    if status.state == "committed" {
        return complete_generation(run, status.next_chunk);
    }
    if status.state != "open" {
        return Err(conflict("generation is terminal and cannot resume"));
    }
    reconcile(run, status.next_chunk)?;
    if status.next_chunk == cursor(run)?.chunk_count {
        return commit(context, run);
    }
    stage(context, run, element, transient, status.next_chunk)
}

fn prepare(
    context: &mut PluginContext<'_>,
    run: &mut RebuildRun,
    element: &SemanticElement,
    transient: &mut TransientWork,
) -> Result<(), PluginError> {
    let plan = ensure_plan(context, run, element, transient)?;
    let vector = text_vectors::embed_chunk(context, plan, 0)?;
    let cursor = prepared_cursor(&run.run_id, element, plan, vector.len())?;
    transient.first_vector = Some((cursor.generation_id.clone(), vector));
    run.generation = Some(cursor);
    Ok(())
}

fn prepared_cursor(
    run_id: &str,
    element: &SemanticElement,
    plan: &text_vectors::PlannedSection,
    dimensions: usize,
) -> Result<GenerationCursor, PluginError> {
    let plan_fingerprint = digest(&plan.ranges)?;
    let generation_id = generation_identity(run_id, element, plan, &plan_fingerprint)?;
    let chunk_count =
        u32::try_from(plan.ranges.len()).map_err(|_| conflict("chunk ordinals must fit u32"))?;
    Ok(GenerationCursor {
        generation_id,
        plan_fingerprint,
        dimensions,
        chunk_count,
        staged_chunks: 0,
    })
}

fn generation_identity(
    run_id: &str,
    element: &SemanticElement,
    plan: &text_vectors::PlannedSection,
    plan_fingerprint: &str,
) -> Result<String, PluginError> {
    digest(&json!([
        run_id,
        element.semantic_element_id,
        element.content_fingerprint,
        plan_fingerprint,
        plan.identity.profile_fingerprint
    ]))
}

fn stage(
    context: &mut PluginContext<'_>,
    run: &mut RebuildRun,
    element: &SemanticElement,
    transient: &mut TransientWork,
    ordinal: u32,
) -> Result<(), PluginError> {
    let vector = chunk_vector(context, run, element, transient, ordinal)?;
    if vector.len() != cursor(run)?.dimensions {
        return Err(conflict("chunk dimensions changed during generation"));
    }
    let input = json!({"key":key(run)?, "chunk":ordinal,"vector":vector});
    let status = generation_call(context, "stage_element_text_vector_chunk", input, run)?;
    if status.state != "open" && status.state != "committed" {
        return Err(conflict("generation closed during staging"));
    }
    reconcile(run, status.next_chunk)
}

fn chunk_vector(
    context: &mut PluginContext<'_>,
    run: &RebuildRun,
    element: &SemanticElement,
    transient: &mut TransientWork,
    ordinal: u32,
) -> Result<Vec<f32>, PluginError> {
    let generation_id = &cursor(run)?.generation_id;
    if let Some((_, vector)) = transient
        .first_vector
        .take()
        .filter(|(id, _)| ordinal == 0 && id == generation_id)
    {
        return Ok(vector);
    }
    let plan = ensure_plan(context, run, element, transient)?;
    text_vectors::embed_chunk(context, plan, ordinal as usize)
}

fn cursor(run: &RebuildRun) -> Result<&GenerationCursor, PluginError> {
    run.generation
        .as_ref()
        .ok_or_else(|| conflict("expected durable generation cursor"))
}

fn commit(context: &mut PluginContext<'_>, run: &mut RebuildRun) -> Result<(), PluginError> {
    let status = generation_call(
        context,
        "commit_element_text_vector_generation",
        json!({"key":key(run)?}),
        run,
    )?;
    if status.state != "committed" || status.published_count != status.chunk_count as usize {
        return Err(conflict("expected complete committed generation"));
    }
    complete_generation(run, status.next_chunk)
}

fn complete_generation(run: &mut RebuildRun, staged: u32) -> Result<(), PluginError> {
    reconcile(run, staged)?;
    finish(run);
    Ok(())
}

fn reconcile(run: &mut RebuildRun, staged: u32) -> Result<(), PluginError> {
    let cursor = run
        .generation
        .as_mut()
        .ok_or_else(|| conflict("expected durable generation cursor"))?;
    if staged < cursor.staged_chunks || staged > cursor.chunk_count {
        return Err(conflict(
            "generation progress moved backwards or beyond plan",
        ));
    }
    run.progress.chunks_staged += (staged - cursor.staged_chunks) as usize;
    cursor.staged_chunks = staged;
    Ok(())
}

fn finish(run: &mut RebuildRun) {
    run.generation = None;
    run.position += 1;
    run.progress.sections_completed += 1;
}

fn spec(
    run: &RebuildRun,
    element: &SemanticElement,
    cursor: &GenerationCursor,
) -> Result<Value, PluginError> {
    let identity = run
        .identity
        .as_ref()
        .ok_or_else(|| conflict("expected pinned model/profile"))?;
    let fingerprint = element
        .content_fingerprint
        .as_ref()
        .ok_or_else(|| conflict("expected full-section fingerprint"))?;
    Ok(
        json!({"key":key(run)?, "content_fingerprint":fingerprint,"engine_id":identity.engine_id,
        "model":identity.model,"profile_fingerprint":identity.profile_fingerprint,"dimensions":cursor.dimensions,
        "chunk_count":cursor.chunk_count,"chunk_plan_fingerprint":cursor.plan_fingerprint}),
    )
}

fn key(run: &RebuildRun) -> Result<Value, PluginError> {
    let target = run
        .targets
        .get(run.position)
        .ok_or_else(|| conflict("expected current section target"))?;
    let cursor = run
        .generation
        .as_ref()
        .ok_or_else(|| conflict("expected generation id"))?;
    Ok(
        json!({"project_root":run.project_root,"semantic_element_id":target.id,"generation_id":cursor.generation_id}),
    )
}

pub(super) fn abort(context: &mut PluginContext<'_>, run: &RebuildRun) -> Result<(), PluginError> {
    if run.generation.is_some() {
        generation_call(
            context,
            "abort_element_text_vector_generation",
            json!({"key":key(run)?}),
            run,
        )?;
    }
    Ok(())
}

fn generation_call(
    context: &mut PluginContext<'_>,
    operation: &str,
    input: Value,
    run: &RebuildRun,
) -> Result<GenerationStatus, PluginError> {
    let value = generation_storage_call(context, operation, input)?;
    let status =
        serde_json::from_value(value).map_err(|_| conflict("expected typed generation status"))?;
    verify_acknowledgement(&status, run, operation)?;
    Ok(status)
}

fn generation_storage_call(
    context: &mut PluginContext<'_>,
    operation: &str,
    mut input: Value,
) -> Result<Value, PluginError> {
    input["operation"] = json!(operation);
    context
        .host_call("storage.semantic", input)
        .map_err(|error| {
            PluginError::new(
                "rebuild_generation_storage_failed",
                "generation storage did not return a confirmed acknowledgement",
                error.retryable,
            )
        })
}

fn verify_acknowledgement(
    status: &GenerationStatus,
    run: &RebuildRun,
    operation: &str,
) -> Result<(), PluginError> {
    let count_matches = operation == "abort_element_text_vector_generation"
        || status.chunk_count == cursor(run)?.chunk_count;
    if status.key == key(run)? && count_matches {
        return Ok(());
    }
    Err(conflict(
        "generation acknowledgement differs from requested element/plan",
    ))
}

fn conflict(message: &str) -> PluginError {
    failure("rebuild_generation_conflict", message)
}
