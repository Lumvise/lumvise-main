//! Maps a provider transport to the native tool-call leak formats known to
//! occur on it. Dispatch keys off `LlmProviderKind` (the structural,
//! reliable discriminant already used for this exact purpose in
//! `config.rs::validate()`), not the free-text `provider_id` config label
//! or a model-name substring match - the XML-tag format has been observed
//! leaking from a `zai-glm-4.7` model on the same Cerebras transport that
//! also leaks the Gemma `call:` format, which already falsifies a naive
//! per-model-name allowlist. `model` is threaded through for a future
//! format that genuinely needs model-level narrowing; today's table does
//! not branch on it.
//!
//! Every known `LlmProviderKind` maps to the same two formats today: a
//! provider's declared `kind` is a transport/protocol label, not a
//! guarantee about which model actually answers behind it (a gateway or
//! proxy can front any vendor-shaped API with a different backend model),
//! and `llm_direct_api_providers.rs::F-009` is an existing, deliberate
//! acceptance criterion that every direct-API provider (Claude, Codex,
//! Gemini) - not just Cerebras/OpenRouter/Zai - recovers a leaked Gemma-
//! native call rather than delivering it verbatim. Both known formats are
//! cheap, mutually exclusive, and self-describing (`call:` vs
//! `<tool_call>`), and each format's own parser is what protects against
//! false positives via its fail-closed-on-ambiguity contract - so there is
//! no correctness cost to applying both everywhere.
//!
//! This match still earns its keep over a constant: it stays exhaustive
//! over `LlmProviderKind`, so adding a new provider kind to `config.rs`
//! forces an explicit decision here rather than silently inheriting
//! whatever the default happens to be. The per-LLM extensibility point the
//! architecture provides is real, just not yet exercised - the day a
//! format is found that is provably unsafe to scan for on some kind (a
//! plausible false-positive against that vendor's ordinary prose), that
//! kind gets its own arm without touching any call site.

use crate::config::LlmProviderKind;

use super::format::LlmCallFormat;

pub(super) fn applicable_formats(kind: LlmProviderKind, _model: &str) -> &'static [LlmCallFormat] {
    use LlmCallFormat::{GemmaCallPrefix, HermesXmlTag};
    match kind {
        LlmProviderKind::Cerebras
        | LlmProviderKind::OpenRouter
        | LlmProviderKind::Zai
        | LlmProviderKind::OpenAiCompatible
        | LlmProviderKind::Claude
        | LlmProviderKind::Codex
        | LlmProviderKind::Gemini
        | LlmProviderKind::OpenAiRealtime
        | LlmProviderKind::Local => &[GemmaCallPrefix, HermesXmlTag],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KINDS: [LlmProviderKind; 9] = [
        LlmProviderKind::Cerebras,
        LlmProviderKind::Claude,
        LlmProviderKind::Codex,
        LlmProviderKind::Gemini,
        LlmProviderKind::OpenAiCompatible,
        LlmProviderKind::OpenAiRealtime,
        LlmProviderKind::OpenRouter,
        LlmProviderKind::Zai,
        LlmProviderKind::Local,
    ];

    #[test]
    fn every_known_provider_kind_gets_both_known_formats_in_gemma_first_order() {
        for kind in ALL_KINDS {
            assert_eq!(
                applicable_formats(kind, "any-model"),
                &[LlmCallFormat::GemmaCallPrefix, LlmCallFormat::HermesXmlTag],
                "kind: {kind:?}"
            );
        }
    }

    #[test]
    fn model_never_narrows_the_format_list_for_any_known_kind() {
        let models = ["gpt-5", "gemma-4-31b", "zai-glm-4.7", "claude-opus-5"];
        for kind in ALL_KINDS {
            for model in models {
                assert_eq!(
                    applicable_formats(kind, model),
                    &[LlmCallFormat::GemmaCallPrefix, LlmCallFormat::HermesXmlTag],
                    "kind: {kind:?}, model: {model}"
                );
            }
        }
    }
}
