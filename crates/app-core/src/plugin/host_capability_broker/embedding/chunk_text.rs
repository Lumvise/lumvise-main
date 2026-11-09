//! Exact source-range planning; source content never appears in capability errors.
use super::*;
use lumvise_neural_core::text2vector::TextChunkPlanRequest;

pub(in crate::plugin::host_capability_broker) fn invoke_neural_chunk_text(
    shared: &SharedPluginVectorizer,
    input: Value,
) -> Result<Value, HostCapabilityError> {
    let request = chunk_request(input)?;
    let selected = selected_service(shared, NEURAL_CHUNK_TEXT_CAPABILITY)?;
    let planner = local_planner(&selected)?;
    let plan = planner.plan_text_chunks(&request).map_err(|_| {
        chunk_error(
            "text_chunk_planning_failed",
            "expected source-preserving token ranges within the requested token budget",
        )
    })?;
    serde_json::to_value(plan).map_err(|_| {
        chunk_error(
            "invalid_text_chunk_plan",
            "expected serializable UTF-8 token ranges",
        )
    })
}

fn local_planner(
    service: &PluginEmbeddingService,
) -> Result<&dyn lumvise_neural_core::text2vector::TextChunkPlanner, HostCapabilityError> {
    let planner = service.planner.as_deref().ok_or_else(|| {
        chunk_error(
            "unsupported_text_chunk_planner",
            "configured backend has no owned local tokenizer",
        )
    })?;
    planner.embedding_profile_fingerprint().map_err(|_| {
        chunk_error(
            "unsupported_embedding_profile",
            "configured backend cannot attest owned local assets",
        )
    })?;
    Ok(planner)
}

fn chunk_request(input: Value) -> Result<TextChunkPlanRequest, HostCapabilityError> {
    let valid = input.as_object().is_some_and(|fields| {
        fields.len() == 3
            && fields
                .keys()
                .all(|key| matches!(key.as_str(), "text" | "task" | "max_tokens"))
    });
    if !valid {
        return Err(chunk_error(
            "invalid_host_capability_input",
            "expected object with text, task and max_tokens",
        ));
    }
    serde_json::from_value(input).map_err(|_| {
        chunk_error(
            "invalid_host_capability_input",
            "expected UTF-8 text, document/query task and nonnegative integer max_tokens",
        )
    })
}

fn chunk_error(code: &str, message: &str) -> HostCapabilityError {
    HostCapabilityError::new(NEURAL_CHUNK_TEXT_CAPABILITY, code, message, false)
}
