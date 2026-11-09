//! Uses a cloned, untruncated tokenizer while preserving the inference configuration.
use super::token_ranges::{TokenMeasurement, plan_ranges};
use super::*;

impl TextChunkPlanner for FastEmbedText2VectorEngine {
    fn embedding_profile_fingerprint(&self) -> Result<String> {
        self.profile_fingerprint
            .clone()
            .ok_or_else(|| NeuralError::InvalidValue {
                value: "builtin remote FastEmbed assets".into(),
                expected: "owned local ONNX assets for supported profile attestation".into(),
            })
    }

    fn plan_text_chunks(&self, request: &TextChunkPlanRequest) -> Result<TextChunkPlan> {
        let profile_fingerprint = self.embedding_profile_fingerprint()?;
        let ranges = self.source_token_ranges(request)?;
        Ok(TextChunkPlan {
            engine_id: self.vector_engine_id.clone(),
            model: Some(self.config.model_id.clone()),
            profile_fingerprint,
            ranges,
        })
    }
}

impl FastEmbedText2VectorEngine {
    fn planning_model(&self) -> Result<std::sync::MutexGuard<'_, TextEmbedding>> {
        self.model
            .lock()
            .map_err(|_| planning_error("tokenizer mutex"))
    }

    fn source_token_ranges(
        &self,
        request: &TextChunkPlanRequest,
    ) -> Result<Vec<crate::text2vector::TextChunkRange>> {
        let mut tokenizer = self.planning_model()?.tokenizer.clone();
        let budget = tokenizer
            .get_truncation()
            .map_or(request.max_tokens, |setting| {
                setting.max_length.min(request.max_tokens)
            });
        tokenizer
            .with_truncation(None)
            .map_err(|_| planning_error("disable truncation"))?;
        tokenizer.with_padding(None);
        let prefix = self.profile.prepare("", request.task);
        let measure = |source: &str| {
            let prepared = format!("{prefix}{source}");
            let encoded = tokenizer
                .encode(prepared.as_str(), true)
                .map_err(|_| planning_error("encode source slice"))?;
            Ok(source_token_offsets(
                encoded.get_offsets(),
                prefix.len(),
                source.len(),
            ))
        };
        plan_ranges(&request.text, budget, measure)
    }
}

fn source_token_offsets(
    offsets: &[(usize, usize)],
    prefix_length: usize,
    source_length: usize,
) -> TokenMeasurement {
    TokenMeasurement {
        count: offsets.len(),
        source_ends: offsets
            .iter()
            .map(|(_, end)| end.saturating_sub(prefix_length).min(source_length))
            .collect(),
    }
}

fn planning_error(operation: &str) -> NeuralError {
    NeuralError::ProviderFailed {
        provider_id: "fastembed-tokenizer".into(),
        message: format!(
            "token planning failed at {operation}; expected an available source-preserving tokenizer"
        ),
    }
}
