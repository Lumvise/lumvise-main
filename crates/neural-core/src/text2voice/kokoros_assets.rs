use crate::error::{NeuralError, Result};
use crate::model_assets::resolve_or_download_cache_file;
use crate::process::ProcessFailureDiagnostics;
use std::path::{Path, PathBuf};
use std::process::Command;

const KOKOROS_CACHE_GROUP: &str = "kokoros";
const SHERPA_KOKORO_ALIAS: &str = "kokoro-v1.0";
const SHERPA_KOKORO_BUNDLE_DIR: &str = "kokoro-en-v0_19";
const SHERPA_KOKORO_BUNDLE_FILE: &str = "kokoro-en-v0_19.tar.bz2";
const SHERPA_KOKORO_BUNDLE_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-en-v0_19.tar.bz2";

pub(crate) struct KokorosSherpaAssets {
    pub(crate) model_path: PathBuf,
    pub(crate) voices_path: PathBuf,
    pub(crate) tokens_path: PathBuf,
    pub(crate) data_dir: PathBuf,
}

pub(crate) fn default_model_alias() -> &'static str {
    SHERPA_KOKORO_ALIAS
}

pub(crate) fn resolve_kokoros_assets(
    model_value: &str,
    voices_value: &str,
) -> Result<KokorosSherpaAssets> {
    if model_key(model_value) == SHERPA_KOKORO_ALIAS {
        return default_kokoros_assets();
    }
    explicit_kokoros_assets(model_value, voices_value)
}

fn default_kokoros_assets() -> Result<KokorosSherpaAssets> {
    let archive = resolve_or_download_cache_file(
        KOKOROS_CACHE_GROUP,
        SHERPA_KOKORO_BUNDLE_FILE,
        SHERPA_KOKORO_BUNDLE_URL,
    )?;
    let parent = archive_parent(&archive)?;
    let bundle_dir = parent.join(SHERPA_KOKORO_BUNDLE_DIR);
    ensure_bundle_extracted(&archive, &parent, &bundle_dir)?;
    assets_from_bundle_dir(&bundle_dir)
}

fn explicit_kokoros_assets(model_value: &str, voices_value: &str) -> Result<KokorosSherpaAssets> {
    let model_path = expand_home(model_value)?;
    let bundle_dir = model_parent(&model_path)?.to_path_buf();
    let voices_path = explicit_voices_path(&bundle_dir, voices_value)?;
    Ok(KokorosSherpaAssets {
        model_path,
        voices_path,
        tokens_path: bundle_dir.join("tokens.txt"),
        data_dir: bundle_dir.join("espeak-ng-data"),
    })
}

fn model_parent(model_path: &Path) -> Result<&Path> {
    model_path
        .parent()
        .ok_or_else(|| NeuralError::MissingValue {
            value: model_path.display().to_string(),
            expected: "parent directory containing Sherpa Kokoro assets".to_string(),
        })
}

fn explicit_voices_path(bundle_dir: &Path, voices_value: &str) -> Result<PathBuf> {
    let path = expand_home(voices_value)?;
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(bundle_dir.join(path))
}

fn ensure_bundle_extracted(archive: &Path, parent: &Path, bundle_dir: &Path) -> Result<()> {
    if bundle_dir.join("model.onnx").is_file() {
        return Ok(());
    }
    let output = Command::new("tar")
        .arg("-xjf")
        .arg(archive)
        .arg("-C")
        .arg(parent)
        .output()
        .map_err(|source| NeuralError::Io {
            value: archive.display().to_string(),
            expected: "extractable Sherpa Kokoro tar.bz2 archive".to_string(),
            source,
        })?;
    require_successful_extract(output.status, output.stderr, archive)
}

fn require_successful_extract(
    status: std::process::ExitStatus,
    stderr: Vec<u8>,
    archive: &Path,
) -> Result<()> {
    if status.success() {
        return Ok(());
    }
    Err(NeuralError::ProcessFailed {
        command: format!("tar -xjf {}", archive.display()),
        status: status.to_string(),
        diagnostics: ProcessFailureDiagnostics::from_streams(&[], &stderr),
    })
}

fn assets_from_bundle_dir(bundle_dir: &Path) -> Result<KokorosSherpaAssets> {
    let assets = KokorosSherpaAssets {
        model_path: bundle_dir.join("model.onnx"),
        voices_path: bundle_dir.join("voices.bin"),
        tokens_path: bundle_dir.join("tokens.txt"),
        data_dir: bundle_dir.join("espeak-ng-data"),
    };
    require_assets_exist(&assets)?;
    Ok(assets)
}

fn require_assets_exist(assets: &KokorosSherpaAssets) -> Result<()> {
    require_file(&assets.model_path, "Sherpa Kokoro model.onnx")?;
    require_file(&assets.voices_path, "Sherpa Kokoro voices.bin")?;
    require_file(&assets.tokens_path, "Sherpa Kokoro tokens.txt")?;
    require_dir(&assets.data_dir, "Sherpa Kokoro espeak-ng-data directory")
}

fn require_file(path: &Path, expected: &str) -> Result<()> {
    if path.is_file() {
        return Ok(());
    }
    Err(NeuralError::MissingValue {
        value: path.display().to_string(),
        expected: expected.to_string(),
    })
}

fn require_dir(path: &Path, expected: &str) -> Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    Err(NeuralError::MissingValue {
        value: path.display().to_string(),
        expected: expected.to_string(),
    })
}

fn archive_parent(archive: &Path) -> Result<PathBuf> {
    archive
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| NeuralError::MissingValue {
            value: archive.display().to_string(),
            expected: "parent directory for Sherpa Kokoro archive".to_string(),
        })
}

fn model_key(value: &str) -> String {
    Path::new(value)
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .unwrap_or(value)
        .trim()
        .to_ascii_lowercase()
}

fn expand_home(value: &str) -> Result<PathBuf> {
    if let Some(rest) = value.strip_prefix("~/") {
        return Ok(cache_home()?.join(rest));
    }
    Ok(PathBuf::from(value))
}

fn cache_home() -> Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| NeuralError::MissingValue {
            value: "HOME".to_string(),
            expected: "home directory for model path expansion".to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_key_uses_file_name_and_lowercase() {
        assert_eq!(model_key("/models/KOKORO-V1.0"), SHERPA_KOKORO_ALIAS);
    }

    #[test]
    fn explicit_voices_path_keeps_absolute_path() {
        let voices = explicit_voices_path(Path::new("/bundle"), "/voices.bin").unwrap();

        assert_eq!(voices, PathBuf::from("/voices.bin"));
    }

    #[test]
    fn explicit_voices_path_resolves_relative_to_bundle() {
        let voices = explicit_voices_path(Path::new("/bundle"), "voices.bin").unwrap();

        assert_eq!(voices, PathBuf::from("/bundle/voices.bin"));
    }
}
