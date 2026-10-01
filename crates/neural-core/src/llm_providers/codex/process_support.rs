use super::CODEX_TEMP_SEQUENCE;
use crate::config::LlmProviderConfig;
use crate::error::{NeuralError, Result};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn write_prompt_file(prompt: &str) -> Result<PathBuf> {
    let path = unique_output_path("codex-prompt");
    std::fs::write(&path, prompt).map_err(|source| NeuralError::Io {
        value: path.display().to_string(),
        expected: "temporary Codex prompt file".to_string(),
        source,
    })?;
    Ok(path)
}

pub(super) fn unique_output_path(prefix: &str) -> PathBuf {
    let sequence = CODEX_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{nanos}-{sequence}",
        std::process::id()
    ))
}

pub(super) fn shell_quote(input: &str) -> String {
    format!("'{}'", input.replace('\'', "'\\''"))
}

pub(super) fn missing_spawn(provider: &str) -> NeuralError {
    NeuralError::MissingValue {
        value: "spawn".to_string(),
        expected: format!("{provider} provider spawn config"),
    }
}

pub(super) fn missing_pipe(pipe: &str) -> NeuralError {
    NeuralError::MissingValue {
        value: pipe.to_string(),
        expected: "codex hot process pipe".to_string(),
    }
}

pub(super) fn hot_closed(marker: &str) -> NeuralError {
    NeuralError::ProcessFailed {
        command: "codex hot session".to_string(),
        status: "closed".to_string(),
        diagnostics: crate::process::ProcessFailureDiagnostics::from_stderr(format!(
            "stream closed before marker `{marker}`"
        )),
    }
}

pub(super) fn decorate_hot_error(config: &LlmProviderConfig, error: NeuralError) -> NeuralError {
    NeuralError::ProviderFailed {
        provider_id: config.provider_id.clone(),
        message: error.to_string(),
    }
}
