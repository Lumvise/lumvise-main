//! Owns real multi-text inference and output validation; model/profile ownership stays in the engine.
use super::*;

const INFERENCE_BATCH_SIZE: usize = 16;

impl FastEmbedText2VectorEngine {
    pub(super) fn embed_text_batch(
        &self,
        texts: &[&str],
        task: TextEmbeddingTask,
    ) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let prepared = prepare_batch(&self.profile, texts, task)?;
        self.inference_groups(&prepared)
    }

    fn inference_groups(&self, prepared: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut vectors = Vec::with_capacity(prepared.len());
        for group in prepared.chunks(inference_group_size(&self.config)) {
            // Dynamic quantization requires its whole input as one batch; group before calling FastEmbed.
            let output = self.infer_group(group)?;
            validate_batch(&output, group.len())?;
            for vector in output {
                vectors.push(self.profile.finish(vector)?);
            }
        }
        Ok(vectors)
    }

    fn infer_group(&self, group: &[String]) -> Result<Vec<Vec<f32>>> {
        // Release between scalar fallbacks so an interactive query can still acquire the model.
        let mut model = self
            .model
            .lock()
            .map_err(|_| self.batch_failure("FastEmbed model mutex poisoned"))?;
        model
            .embed(group, None)
            .map_err(|error| self.batch_failure(error.to_string()))
    }

    fn batch_failure(&self, message: impl Into<String>) -> NeuralError {
        NeuralError::ProviderFailed {
            provider_id: self.config.engine_id.clone(),
            message: message.into(),
        }
    }
}

fn inference_group_size(config: &FastEmbedText2VectorConfig) -> usize {
    // shortcut: local ONNX assets lack trusted quantization metadata; keep scalar until the catalog supplies it.
    if config.model_dir.is_some() {
        return 1;
    }
    match resolve_fastembed_model(&config.model_id)
        .map(|model| TextEmbedding::get_quantization_mode(&model))
    {
        Ok(fastembed::QuantizationMode::None | fastembed::QuantizationMode::Static) => {
            INFERENCE_BATCH_SIZE
        }
        _ => 1,
    }
}

fn prepare_batch(
    profile: &TextProfile,
    texts: &[&str],
    task: TextEmbeddingTask,
) -> Result<Vec<String>> {
    for text in texts {
        require_non_empty(text, "non-empty text to vectorize")?;
    }
    Ok(texts
        .iter()
        .map(|text| profile.prepare(text, task))
        .collect())
}

fn validate_batch(vectors: &[Vec<f32>], expected: usize) -> Result<()> {
    let dimensions = vectors.first().map_or(0, Vec::len);
    if vectors.len() == expected
        && dimensions > 0
        && vectors.iter().all(|vector| {
            vector.len() == dimensions && vector.iter().all(|value| value.is_finite())
        })
    {
        return Ok(());
    }
    Err(NeuralError::MalformedPayload {
        value: format!("{} vectors, first dimension {dimensions}", vectors.len()),
        expected: format!("{expected} ordered, finite, non-empty equal-dimensional vectors"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unattested_local_models_keep_scalar_compatibility_and_builtin_float_models_batch() {
        assert_eq!(
            inference_group_size(&FastEmbedText2VectorConfig::local_onnx(
                "local",
                "nomic-embed-text-v1.5",
                "/installed".into()
            )),
            1
        );
        assert_eq!(
            inference_group_size(&FastEmbedText2VectorConfig::builtin(
                "builtin",
                "all-minilm-l6-v2"
            )),
            16
        );
        assert_eq!(
            inference_group_size(&FastEmbedText2VectorConfig::builtin(
                "unknown",
                "unknown-model"
            )),
            1
        );
    }
    #[test]
    fn each_input_keeps_its_task_prefix_without_reordering() {
        assert_eq!(
            prepare_batch(
                &NOMIC_TEXT,
                &["second", "first"],
                TextEmbeddingTask::Document
            )
            .unwrap(),
            ["search_document: second", "search_document: first"]
        );
        assert_eq!(
            prepare_batch(&NOMIC_TEXT, &["second", "first"], TextEmbeddingTask::Query).unwrap(),
            ["search_query: second", "search_query: first"]
        );
        assert!(prepare_batch(&PLAIN_TEXT, &["first", "  "], TextEmbeddingTask::Document).is_err());
    }
    #[test]
    fn incorrect_cardinality_dimensions_and_nonfinite_values_are_rejected() {
        assert!(validate_batch(&[vec![1.0]], 2).is_err());
        assert!(validate_batch(&[vec![]], 1).is_err());
        assert!(validate_batch(&[vec![1.0], vec![1.0, 2.0]], 2).is_err());
        assert!(validate_batch(&[vec![f32::NAN]], 1).is_err());
        assert!(validate_batch(&[vec![1.0], vec![2.0]], 2).is_ok());
    }
}
