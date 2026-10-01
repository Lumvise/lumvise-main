use crate::error::Result;
use crate::model_assets::{DownloadableModel, resolve_or_download_model};
use std::path::{Path, PathBuf};

const WHISPER_CACHE_GROUP: &str = "whisper-rs";
const WHISPER_MODELS: &[DownloadableModel] = &[
    DownloadableModel {
        alias: "tiny.en",
        file_name: "ggml-tiny.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
    },
    DownloadableModel {
        alias: "base.en",
        file_name: "ggml-base.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
    },
    DownloadableModel {
        alias: "small.en",
        file_name: "ggml-small.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
    },
    DownloadableModel {
        alias: "large-v3-turbo",
        file_name: "ggml-large-v3-turbo.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
    },
];

pub(crate) fn resolve_whisper_model_path(value: &str) -> Result<PathBuf> {
    resolve_or_download_model(value, WHISPER_CACHE_GROUP, WHISPER_MODELS)
}

pub(crate) fn path_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}
