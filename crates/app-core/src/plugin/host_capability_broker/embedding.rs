//! neural.embed validation and vectorizer execution; storage routing stays in the broker.
use super::{NEURAL_EMBED_CAPABILITY, SharedPluginVectorizer};
use lumvise_db_core::{ArtifactTextVectorizer, TextEmbeddingTask};
use lumvise_plugin_runtime::HostCapabilityError;
use serde_json::{Map, Value, json};

pub(super) const MAX_EMBED_TEXTS: usize = 128;
const EMBED_INPUT_SHAPE: &str = "object with field `texts` and optional field `task`";
const MAX_EMBED_TEXT_BYTES: usize = 32 * 1024;
const MAX_EMBED_TOTAL_BYTES: usize = 256 * 1024;
const MAX_EMBED_DIMENSIONS: usize = 4_096;

pub(super) fn invoke_neural_embed(
    vectorizer: &SharedPluginVectorizer,
    input: Value,
) -> Result<Value, HostCapabilityError> {
    let (texts, task) = embed_texts(&input)?;
    let vectorizer = vectorizer
        .lock()
        .map_err(|_| embed_failure("plugin vectorizer mutex poisoned"))?
        .clone()
        .ok_or_else(|| embed_unavailable("no text vectorizer is configured"))?;
    let embedded = texts
        .iter()
        .map(|text| embed_one(vectorizer.as_ref(), text, task))
        .collect::<Result<Vec<_>, _>>()?;
    let vectors = embedded
        .iter()
        .map(|result| result.vector.clone())
        .collect::<Vec<_>>();
    validate_vectors(&vectors, texts.len())?;
    let (engine_id, model) = embedded
        .first()
        .map(|result| (result.engine_id.clone(), result.model.clone()))
        .unwrap_or_default();
    Ok(json!({"vectors": vectors, "engine_id": engine_id, "model": model}))
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
    validate_total_text_bytes(&texts, &fields["texts"])?;
    Ok((texts, task))
}

fn require_embed_fields(fields: &Map<String, Value>) -> Result<(), HostCapabilityError> {
    if fields.contains_key("texts")
        && fields
            .keys()
            .all(|key| matches!(key.as_str(), "texts" | "task"))
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
        .filter(|text| !text.is_empty() && text.len() <= MAX_EMBED_TEXT_BYTES)
        .ok_or_else(|| {
            invalid_embed_input(
                value,
                &format!("non-empty UTF-8 string no larger than {MAX_EMBED_TEXT_BYTES} bytes"),
            )
        })
}

fn validate_total_text_bytes(texts: &[&str], value: &Value) -> Result<(), HostCapabilityError> {
    let total = texts.iter().map(|text| text.len()).sum::<usize>();
    if total <= MAX_EMBED_TOTAL_BYTES {
        return Ok(());
    }
    Err(invalid_embed_input(
        value,
        &format!("texts totaling no more than {MAX_EMBED_TOTAL_BYTES} UTF-8 bytes"),
    ))
}

fn embed_one(
    vectorizer: &dyn ArtifactTextVectorizer,
    text: &str,
    task: TextEmbeddingTask,
) -> Result<lumvise_db_core::ArtifactTextVector, HostCapabilityError> {
    vectorizer
        .vectorize_artifact_text(text, task)
        .map_err(|error| embed_failure(format!("text vectorization failed: {error}")))
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
        format!("invalid neural.embed input `{value}`; expected {expected}"),
        false,
    )
}

fn embed_unavailable(message: impl Into<String>) -> HostCapabilityError {
    HostCapabilityError::new(
        NEURAL_EMBED_CAPABILITY,
        "host_capability_unavailable",
        message,
        true,
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
