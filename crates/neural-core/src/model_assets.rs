use crate::error::{NeuralError, Result};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(crate) struct DownloadableModel {
    pub alias: &'static str,
    pub file_name: &'static str,
    pub url: &'static str,
}

pub(crate) fn resolve_or_download_model(
    value: &str,
    cache_group: &str,
    catalog: &[DownloadableModel],
) -> Result<PathBuf> {
    let path = expand_home(value)?;
    if path.is_file() {
        return Ok(path);
    }
    let Some(model) = catalog_match(value, catalog) else {
        return Ok(path);
    };
    cached_model_path(cache_group, model).and_then(|path| ensure_cached(&path, model))
}

pub(crate) fn resolve_or_download_cache_file(
    cache_group: &str,
    file_name: &str,
    url: &str,
) -> Result<PathBuf> {
    let path = cache_root()?.join(cache_group).join(file_name);
    ensure_url_cached(&path, url)
}

fn ensure_cached(path: &Path, model: &DownloadableModel) -> Result<PathBuf> {
    ensure_url_cached(path, model.url)
}

fn ensure_url_cached(path: &Path, url: &str) -> Result<PathBuf> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    create_parent_dir(path)?;
    download_to_path(url, path)?;
    Ok(path.to_path_buf())
}

fn cached_model_path(cache_group: &str, model: &DownloadableModel) -> Result<PathBuf> {
    Ok(cache_root()?.join(cache_group).join(model.file_name))
}

fn cache_root() -> Result<PathBuf> {
    if let Ok(value) = std::env::var("LUMVISE_NEURAL_MODEL_CACHE") {
        return Ok(PathBuf::from(value));
    }
    let home = std::env::var("HOME").map_err(|_| NeuralError::MissingValue {
        value: "HOME".to_string(),
        expected: "home directory for Lumvise neural model cache".to_string(),
    })?;
    Ok(PathBuf::from(home).join(".cache/lumvise/neural-core"))
}

fn catalog_match<'a>(
    value: &str,
    catalog: &'a [DownloadableModel],
) -> Option<&'a DownloadableModel> {
    let key = model_key(value);
    catalog
        .iter()
        .find(|model| key == model.alias || key == model.file_name)
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

fn create_parent_dir(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| NeuralError::MissingValue {
        value: path.display().to_string(),
        expected: "model cache parent directory".to_string(),
    })?;
    std::fs::create_dir_all(parent).map_err(|source| NeuralError::Io {
        value: parent.display().to_string(),
        expected: "writable model cache directory".to_string(),
        source,
    })
}

fn download_to_path(url: &str, path: &Path) -> Result<()> {
    let temp_path = path.with_extension("download");
    let started_at = Instant::now();
    tracing::info!(
        target: "neural-core::model-assets",
        event = "model_download_started",
        url,
        destination = %path.display(),
        "downloading model asset",
    );
    let mut response = get_download_response(url)?;
    let mut file = create_download_file(&temp_path)?;
    std::io::copy(&mut response, &mut file).map_err(|source| NeuralError::Io {
        value: temp_path.display().to_string(),
        expected: format!("downloaded model bytes from {url}"),
        source,
    })?;
    std::fs::rename(&temp_path, path).map_err(|source| NeuralError::Io {
        value: path.display().to_string(),
        expected: "atomic model cache rename".to_string(),
        source,
    })?;
    let bytes = std::fs::metadata(path)
        .map_err(|source| NeuralError::Io {
            value: path.display().to_string(),
            expected: "downloaded model file metadata".to_string(),
            source,
        })?
        .len();
    tracing::info!(
        target: "neural-core::model-assets",
        event = "model_download_finished",
        url,
        destination = %path.display(),
        bytes,
        duration_ms = started_at.elapsed().as_millis(),
        "model asset downloaded",
    );
    Ok(())
}

fn get_download_response(url: &str) -> Result<reqwest::blocking::Response> {
    let response = reqwest::blocking::get(url).map_err(download_error)?;
    if response.status().is_success() {
        return Ok(response);
    }
    Err(NeuralError::ProviderFailed {
        provider_id: "model-download".to_string(),
        message: format!("GET {url} returned {}", response.status()),
    })
}

fn create_download_file(path: &Path) -> Result<File> {
    File::create(path).map_err(|source| NeuralError::Io {
        value: path.display().to_string(),
        expected: "writable temporary model download file".to_string(),
        source,
    })
}

fn download_error(error: reqwest::Error) -> NeuralError {
    NeuralError::ProviderFailed {
        provider_id: "model-download".to_string(),
        message: error.to_string(),
    }
}
