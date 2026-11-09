//! neural.embed validation and vectorizer execution; storage routing stays in the broker.
#[cfg(test)]
mod tests;
use super::{
    NEURAL_CHUNK_TEXT_CAPABILITY, NEURAL_EMBED_CAPABILITY, PluginEmbeddingService,
    SharedPluginVectorizer,
};
mod chunk_text;
pub(super) use chunk_text::invoke_neural_chunk_text;
use lumvise_db_core::{ArtifactTextVectorizer, TextEmbeddingTask};
use lumvise_plugin_runtime::HostCapabilityError;
use serde_json::{Map, Value, json};

pub(super) const MAX_EMBED_TEXTS: usize = 128;
const EMBED_INPUT_SHAPE: &str =
    "object with field `texts` and optional fields `task` and `expected_profile_fingerprint`";
const MAX_EMBED_DIMENSIONS: usize = 4_096;

pub(super) fn invoke_neural_embed(
    vectorizer: &SharedPluginVectorizer,
    input: Value,
) -> Result<Value, HostCapabilityError> {
    let (texts, task) = embed_texts(&input)?;
    let selected = selected_service(vectorizer, NEURAL_EMBED_CAPABILITY)?;
    let expected = expected_profile(&input)?;
    let profile_fingerprint = checked_profile(&selected, expected)?;
    let embedded = texts
        .iter()
        .map(|text| embed_one(selected.vectorizer.as_ref(), text, task))
        .collect::<Result<Vec<_>, _>>()?;
    embedded_response(embedded, profile_fingerprint, texts.len())
}

fn embedded_response(
    embedded: Vec<lumvise_db_core::ArtifactTextVector>,
    profile: Option<String>,
    expected: usize,
) -> Result<Value, HostCapabilityError> {
    let (engine_id, model) = embedded
        .first()
        .map(|result| (result.engine_id.clone(), result.model.clone()))
        .unwrap_or_default();
    let vectors = embedded
        .into_iter()
        .map(|result| result.vector)
        .collect::<Vec<_>>();
    validate_vectors(&vectors, expected)?;
    Ok(json!({"vectors":vectors,"engine_id":engine_id,"model":model,"profile_fingerprint":profile}))
}

fn embed_texts(input: &Value) -> Result<(Vec<&str>, TextEmbeddingTask), HostCapabilityError> {
    let fields = input
        .as_object()
        .ok_or_else(|| invalid_embed_input(input, EMBED_INPUT_SHAPE))?;
    require_embed_fields(fields)?;
    let task = embed_task(fields)?;
    let values = fields["texts"]
        .as_array()
        .ok_or_else(|| invalid_embed_input(&fields["texts"], "non-empty array field `texts`"))?;
    validate_embed_count(values.len(), &fields["texts"])?;
    let texts = values
        .iter()
        .map(embed_text)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((texts, task))
}

fn require_embed_fields(fields: &Map<String, Value>) -> Result<(), HostCapabilityError> {
    if fields.contains_key("texts")
        && fields.keys().all(|key| {
            matches!(
                key.as_str(),
                "texts" | "task" | "expected_profile_fingerprint"
            )
        })
    {
        return Ok(());
    }
    Err(invalid_embed_input(
        &Value::Object(fields.clone()),
        EMBED_INPUT_SHAPE,
    ))
}

fn embed_task(fields: &Map<String, Value>) -> Result<TextEmbeddingTask, HostCapabilityError> {
    match fields.get("task") {
        None => Ok(TextEmbeddingTask::Document),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|_| invalid_embed_input(value, "task `document` or `query`")),
    }
}

fn validate_embed_count(count: usize, value: &Value) -> Result<(), HostCapabilityError> {
    if (1..=MAX_EMBED_TEXTS).contains(&count) {
        return Ok(());
    }
    Err(invalid_embed_input(
        value,
        &format!("array containing 1 through {MAX_EMBED_TEXTS} texts"),
    ))
}

fn embed_text(value: &Value) -> Result<&str, HostCapabilityError> {
    value
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| invalid_embed_input(value, "non-empty UTF-8 string"))
}

fn embed_one(
    vectorizer: &dyn ArtifactTextVectorizer,
    text: &str,
    task: TextEmbeddingTask,
) -> Result<lumvise_db_core::ArtifactTextVector, HostCapabilityError> {
    vectorizer
        .vectorize_artifact_text(text, task)
        .map_err(|_| embed_failure(format!("text vectorization failed for {} source bytes; expected a vector from the configured provider", text.len())))
}

fn validate_vectors(vectors: &[Vec<f32>], expected: usize) -> Result<(), HostCapabilityError> {
    let dimensions = vectors.first().map(Vec::len).unwrap_or_default();
    let valid = vectors.len() == expected
        && (1..=MAX_EMBED_DIMENSIONS).contains(&dimensions)
        && vectors.iter().all(|vector| {
            vector.len() == dimensions && vector.iter().all(|value| value.is_finite())
        });
    if valid {
        return Ok(());
    }
    Err(embed_failure(format!(
        "invalid vectorizer output: received {} vectors with first dimension {dimensions}; expected {expected} finite vectors with equal dimensions from 1 through {MAX_EMBED_DIMENSIONS}",
        vectors.len()
    )))
}

fn invalid_embed_input(value: &Value, expected: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        NEURAL_EMBED_CAPABILITY,
        "invalid_host_capability_input",
        format!(
            "invalid neural.embed input ({}); expected {expected}",
            input_shape(value)
        ),
        false,
    )
}

fn embed_failure(message: impl Into<String>) -> HostCapabilityError {
    HostCapabilityError::new(
        NEURAL_EMBED_CAPABILITY,
        "host_capability_execution_failed",
        message,
        true,
    )
}

fn selected_service(
    shared: &SharedPluginVectorizer,
    capability: &str,
) -> Result<PluginEmbeddingService, HostCapabilityError> {
    shared
        .lock()
        .map_err(|_| embed_failure("plugin vectorizer mutex poisoned"))?
        .clone()
        .ok_or_else(|| {
            HostCapabilityError::new(
                capability,
                "host_capability_unavailable",
                "no text vectorizer is configured",
                true,
            )
        })
}

fn expected_profile(input: &Value) -> Result<Option<&str>, HostCapabilityError> {
    match input.get("expected_profile_fingerprint") {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|profile| !profile.is_empty())
            .map(Some)
            .ok_or_else(|| {
                invalid_embed_input(value, "non-empty expected_profile_fingerprint string")
            }),
    }
}

fn checked_profile(
    service: &PluginEmbeddingService,
    expected: Option<&str>,
) -> Result<Option<String>, HostCapabilityError> {
    let profile = service
        .planner
        .as_ref()
        .and_then(|planner| planner.embedding_profile_fingerprint().ok());
    match (expected, profile) {
        (Some(_), None) => Err(unsupported_profile()),
        (Some(expected), Some(actual)) if expected != actual => Err(mismatched_profile()),
        (_, actual) => Ok(actual),
    }
}

fn unsupported_profile() -> HostCapabilityError {
    HostCapabilityError::new(
        NEURAL_EMBED_CAPABILITY,
        "unsupported_embedding_profile",
        "configured backend cannot attest owned local assets",
        false,
    )
}

fn mismatched_profile() -> HostCapabilityError {
    HostCapabilityError::new(
        NEURAL_EMBED_CAPABILITY,
        "embedding_profile_conflict",
        "expected_profile_fingerprint differs from selected immutable embedding service",
        true,
    )
}

fn input_shape(value: &Value) -> String {
    match value {
        Value::String(text) => format!("string of {} bytes", text.len()),
        Value::Array(items) => format!("array of {} items", items.len()),
        Value::Object(fields) => format!("object of {} fields", fields.len()),
        _ => format!("non-text scalar {value}"),
    }
}
