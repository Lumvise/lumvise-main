//! Owns the Canvas document worker's explicit model environment and read grants.
//! Only the sandbox adapter calls this module; parent credentials stay excluded.

use std::{collections::BTreeSet, ffi::OsString, path::PathBuf, process::Command};

const MODEL_ENVIRONMENT: [&str; 7] = [
    "DOCLING_LAYOUT_ONNX",
    "DOCLING_OCR_REC_ONNX",
    "DOCLING_OCR_DICT",
    "DOCLING_TABLEFORMER_ENCODER",
    "DOCLING_TABLEFORMER_DECODER",
    "DOCLING_TABLEFORMER_BBOX",
    "PDFIUM_DYNAMIC_LIB_PATH",
];

pub(super) struct DocumentModelAssets {
    environment: Vec<(&'static str, OsString)>,
    read_directories: BTreeSet<PathBuf>,
}

impl DocumentModelAssets {
    pub(super) fn for_plugin(
        plugin_id: &str,
        read_environment: impl Fn(&str) -> Option<OsString>,
    ) -> Self {
        let environment: Vec<_> = MODEL_ENVIRONMENT
            .iter()
            .filter(|_| plugin_id == "builtin.canvas")
            .filter_map(|name| read_environment(name).map(|value| (*name, value)))
            .filter(|(_, value)| !value.is_empty())
            .collect();
        let read_directories = environment
            .iter()
            .filter_map(|(_, value)| asset_directory(value))
            .collect();
        Self {
            environment,
            read_directories,
        }
    }

    pub(super) fn apply(&self, command: &mut Command, base_profile: &str) {
        let mut profile = base_profile.to_owned();
        command.envs(self.environment.iter().map(|(name, value)| (name, value)));
        for (index, directory) in self.read_directories.iter().enumerate() {
            // ONNX graph sidecars and PDFium's adjacent libraries need the same read grant.
            let parameter = format!("DOCUMENT_ASSET_ROOT_{index}");
            profile.push_str(&format!(
                "\n(allow file-read* (subpath (param \"{parameter}\")))\n"
            ));
            command
                .arg("-D")
                .arg(format!("{parameter}={}", directory.display()));
        }
        command.arg("-p").arg(profile);
    }
}

fn asset_directory(value: &OsString) -> Option<PathBuf> {
    let path = PathBuf::from(value).canonicalize().ok()?;
    if path.is_dir() {
        return Some(path);
    }
    path.parent().map(PathBuf::from)
}

#[cfg(test)]
mod tests;
