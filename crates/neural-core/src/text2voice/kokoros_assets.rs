use crate::error::{NeuralError, Result};
use crate::model_assets::{expand_model_path, resolve_or_download_cache_file};
use bzip2::read::BzDecoder;
use std::fs::File;
use std::path::{Path, PathBuf};

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
    let model_path = expand_model_path(model_value)?;
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
    let path = expand_model_path(voices_value)?;
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(bundle_dir.join(path))
}

fn ensure_bundle_extracted(archive: &Path, parent: &Path, bundle_dir: &Path) -> Result<()> {
    if bundle_dir.join("model.onnx").is_file() {
        return Ok(());
    }
    let file = File::open(archive).map_err(|source| NeuralError::Io {
        value: archive.display().to_string(),
        expected: "extractable Sherpa Kokoro tar.bz2 archive".to_string(),
        source,
    })?;
    tar::Archive::new(BzDecoder::new(file))
        .unpack(parent)
        .map_err(|source| NeuralError::Io {
            value: archive.display().to_string(),
            expected: "extractable Sherpa Kokoro tar.bz2 archive".to_string(),
            source,
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

#[cfg(test)]
mod tests {
    use super::*;
    use bzip2::Compression;
    use bzip2::write::BzEncoder;
    use std::io::Cursor;

    type TestBundleArchive = tar::Builder<BzEncoder<File>>;

    #[test]
    fn model_key_uses_file_name_and_lowercase() {
        assert_eq!(model_key("/models/KOKORO-V1.0"), SHERPA_KOKORO_ALIAS);
    }

    #[test]
    fn explicit_voices_path_keeps_absolute_path() {
        let absolute_path = std::env::temp_dir().join("voices.bin");
        let voices =
            explicit_voices_path(Path::new("/bundle"), absolute_path.to_str().unwrap()).unwrap();

        assert_eq!(voices, absolute_path);
    }

    #[test]
    fn explicit_voices_path_resolves_relative_to_bundle() {
        let voices = explicit_voices_path(Path::new("/bundle"), "voices.bin").unwrap();

        assert_eq!(voices, PathBuf::from("/bundle/voices.bin"));
    }

    #[test]
    fn explicit_kokoro_model_and_voices_share_home_expansion() {
        let assets = explicit_kokoros_assets("~/models/model.onnx", "~/voices/voices.bin").unwrap();

        assert_eq!(
            assets.model_path,
            expand_model_path("~/models/model.onnx").unwrap()
        );
        assert_eq!(
            assets.voices_path,
            expand_model_path("~/voices/voices.bin").unwrap()
        );
    }

    #[test]
    fn ensure_bundle_extracted_unpacks_required_assets_from_tar_bz2() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("kokoro.tar.bz2");
        write_test_bundle_archive(&archive);
        let bundle_dir = temp.path().join(SHERPA_KOKORO_BUNDLE_DIR);

        ensure_bundle_extracted(&archive, temp.path(), &bundle_dir).unwrap();

        assets_from_bundle_dir(&bundle_dir).unwrap();
    }

    #[test]
    fn ensure_bundle_extracted_reports_missing_archive_path_and_shape() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("missing.tar.bz2");
        let bundle_dir = temp.path().join(SHERPA_KOKORO_BUNDLE_DIR);

        let error = ensure_bundle_extracted(&archive, temp.path(), &bundle_dir).unwrap_err();

        assert!(error.to_string().contains(&archive.display().to_string()));
        assert!(error.to_string().contains("Sherpa Kokoro tar.bz2 archive"));
    }

    #[test]
    fn ensure_bundle_extracted_reports_corrupt_archive_path_and_shape() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("corrupt.tar.bz2");
        std::fs::write(&archive, b"not a bzip2 archive").unwrap();
        let bundle_dir = temp.path().join(SHERPA_KOKORO_BUNDLE_DIR);

        let error = ensure_bundle_extracted(&archive, temp.path(), &bundle_dir).unwrap_err();

        assert!(error.to_string().contains(&archive.display().to_string()));
        assert!(error.to_string().contains("Sherpa Kokoro tar.bz2 archive"));
    }

    #[test]
    fn ensure_bundle_extracted_reuses_existing_bundle_without_opening_archive() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("missing.tar.bz2");
        let bundle_dir = temp.path().join(SHERPA_KOKORO_BUNDLE_DIR);
        write_test_bundle_files(&bundle_dir);

        ensure_bundle_extracted(&archive, temp.path(), &bundle_dir).unwrap();

        assets_from_bundle_dir(&bundle_dir).unwrap();
    }

    fn write_test_bundle_archive(path: &Path) {
        let file = File::create(path).unwrap();
        let encoder = BzEncoder::new(file, Compression::best());
        let mut builder = tar::Builder::new(encoder);
        append_test_bundle_file(&mut builder, "model.onnx", b"model");
        append_test_bundle_file(&mut builder, "voices.bin", b"voices");
        append_test_bundle_file(&mut builder, "tokens.txt", b"tokens");
        append_test_bundle_dir(&mut builder, "espeak-ng-data");
        builder.into_inner().unwrap().finish().unwrap();
    }

    fn append_test_bundle_file(builder: &mut TestBundleArchive, name: &str, bytes: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                format!("{SHERPA_KOKORO_BUNDLE_DIR}/{name}"),
                Cursor::new(bytes),
            )
            .unwrap();
    }

    fn append_test_bundle_dir(builder: &mut TestBundleArchive, name: &str) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                format!("{SHERPA_KOKORO_BUNDLE_DIR}/{name}"),
                Cursor::new(Vec::<u8>::new()),
            )
            .unwrap();
    }

    fn write_test_bundle_files(bundle_dir: &Path) {
        std::fs::create_dir_all(bundle_dir.join("espeak-ng-data")).unwrap();
        std::fs::write(bundle_dir.join("model.onnx"), b"model").unwrap();
        std::fs::write(bundle_dir.join("voices.bin"), b"voices").unwrap();
        std::fs::write(bundle_dir.join("tokens.txt"), b"tokens").unwrap();
    }
}
