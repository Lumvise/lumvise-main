//! Hashes the exact local asset bytes handed to FastEmbed, not mutable cache labels.
use super::{FastEmbedText2VectorConfig, TextProfile};
use fastembed::{Pooling, TokenizerFiles};
use sha2::{Digest, Sha256};

pub(super) fn local_asset_digest(weights: &[u8], tokenizer: &TokenizerFiles) -> String {
    let mut digest = Sha256::new();
    for bytes in [
        weights,
        &tokenizer.tokenizer_file,
        &tokenizer.config_file,
        &tokenizer.special_tokens_map_file,
        &tokenizer.tokenizer_config_file,
    ] {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}

pub(super) fn profile_digest(
    config: &FastEmbedText2VectorConfig,
    profile: &TextProfile,
    pooling: Pooling,
    asset_digest: &str,
    configured_tokenizer: &str,
) -> String {
    let settings = serde_json::json!({
        "assets": asset_digest, "tokenizer": configured_tokenizer,
        "model": config.model_id, "engine_label": config.engine_id, "pooling": format!("{pooling:?}"),
        "document_prefix": profile.document_prefix, "query_prefix": profile.query_prefix,
        "dimensions": profile.output_dimensions, "normalization": if profile.output_dimensions.is_some() { "layernorm-1e-5-cut-l2-v1" } else { "native-fastembed-v1" },
    });
    format!("{:x}", Sha256::digest(settings.to_string().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn asset_identity_changes_with_weight_and_tokenizer_bytes() {
        let mut tokenizer = TokenizerFiles {
            tokenizer_file: vec![1],
            config_file: vec![2],
            special_tokens_map_file: vec![3],
            tokenizer_config_file: vec![4],
        };
        let original = local_asset_digest(&[7, 8], &tokenizer);
        assert_ne!(original, local_asset_digest(&[7, 9], &tokenizer));
        tokenizer.tokenizer_file.push(5);
        assert_ne!(original, local_asset_digest(&[7, 8], &tokenizer));
    }
}
