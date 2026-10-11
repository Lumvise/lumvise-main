//! Private Windows companion payload owned by release publication.
//! Only the publisher may read and stage this payload; signing stays in the packager.

use std::{collections::BTreeMap, fs, path::Path};

use lumvise_plugin_package::PluginManifest;

use crate::{BuiltinReleaseError, BuiltinReleaseMatrix, Result};

const WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";

pub(super) struct WindowsRuntimePayload {
    libraries: BTreeMap<String, Vec<u8>>,
}

impl WindowsRuntimePayload {
    pub(super) fn read(matrix: &BuiltinReleaseMatrix, directory: Option<&Path>) -> Result<Self> {
        let Some(directory) = directory else {
            return Ok(Self {
                libraries: BTreeMap::new(),
            });
        };
        if !matrix.targets.contains_key(WINDOWS_TARGET) {
            return Err(invalid(&directory.display().to_string(), WINDOWS_TARGET));
        }
        Self::read_directory(directory)
    }

    fn read_directory(directory: &Path) -> Result<Self> {
        require_directory(directory)?;
        let mut payload = Self {
            libraries: BTreeMap::new(),
        };
        let entries = fs::read_dir(directory).map_err(|error| super::io_error(directory, error))?;
        for entry in entries {
            let entry = entry.map_err(|error| super::io_error(directory, error))?;
            payload.read_entry(&entry.path())?;
        }
        require_libraries(directory, &payload.libraries)?;
        Ok(payload)
    }

    fn read_entry(&mut self, path: &Path) -> Result<()> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid(&path.display().to_string(), "UTF-8 Windows DLL basename"))?;
        if !dll_candidate(name) {
            return Ok(());
        }
        require_filename(name)?;
        let bytes = read_library(path)?;
        self.insert_library(name, bytes)
    }

    fn insert_library(&mut self, name: &str, bytes: Vec<u8>) -> Result<()> {
        if self
            .libraries
            .keys()
            .any(|existing| existing.to_lowercase() == name.to_lowercase())
        {
            return Err(invalid(name, "case-insensitively unique DLL basenames"));
        }
        self.libraries.insert(name.to_owned(), bytes);
        Ok(())
    }

    pub(super) fn stage(&self, manifest: &mut PluginManifest, staging: &Path) -> Result<()> {
        if self.libraries.is_empty() {
            return Ok(());
        }
        let executable = manifest
            .targets
            .get(WINDOWS_TARGET)
            .ok_or_else(|| invalid(&manifest.plugin_id, "resolved Windows executable path"))?;
        let paths = self.library_paths(executable);
        require_free_paths(&paths, manifest)?;
        for (path, bytes) in paths {
            super::write_payload(bytes, staging, &path)?;
            manifest.files.insert(path, super::hex_digest(bytes));
        }
        Ok(())
    }

    fn library_paths(&self, executable: &str) -> Vec<(String, &[u8])> {
        let parent = executable.rsplit_once('/').map(|(parent, _)| parent);
        self.libraries
            .iter()
            .map(|(name, bytes)| {
                let path = parent.map_or_else(|| name.clone(), |parent| format!("{parent}/{name}"));
                (path, bytes.as_slice())
            })
            .collect()
    }
}

fn require_libraries(directory: &Path, libraries: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    if libraries.is_empty() {
        return Err(invalid(
            &directory.display().to_string(),
            "at least one nonempty regular *.dll",
        ));
    }
    Ok(())
}

fn require_directory(directory: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(directory).map_err(|error| super::io_error(directory, error))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        return Ok(());
    }
    Err(invalid(
        &directory.display().to_string(),
        "regular non-symlink DLL directory",
    ))
}

fn dll_candidate(name: &str) -> bool {
    // Reject malformed trailing dots/spaces rather than quietly dropping a reviewed DLL.
    name.trim_end_matches(['.', ' '])
        .to_ascii_lowercase()
        .ends_with(".dll")
}

fn require_filename(name: &str) -> Result<()> {
    let invalid_character = name
        .chars()
        .any(|character| character <= '\u{1f}' || "<>:\"/\\|?*".contains(character));
    if name.len() <= 4
        || !name.to_ascii_lowercase().ends_with(".dll")
        || invalid_character
        || reserved_device(name)
    {
        return Err(invalid(
            name,
            "non-reserved Windows DLL basename without forbidden characters or trailing dots/spaces",
        ));
    }
    Ok(())
}

fn reserved_device(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let suffix = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"));
    suffix.is_some_and(reserved_port_suffix)
}

fn reserved_port_suffix(suffix: &str) -> bool {
    matches!(
        suffix,
        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
    )
}

fn read_library(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).map_err(|error| super::io_error(path, error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid(
            &path.display().to_string(),
            "regular non-symlink DLL file",
        ));
    }
    let bytes = fs::read(path).map_err(|error| super::io_error(path, error))?;
    if bytes.is_empty() {
        return Err(invalid(&path.display().to_string(), "nonempty DLL file"));
    }
    Ok(bytes)
}

fn require_free_paths(paths: &[(String, &[u8])], manifest: &PluginManifest) -> Result<()> {
    for (path, _) in paths {
        if let Some(existing) = manifest
            .files
            .keys()
            .find(|existing| paths_collide(path, existing))
        {
            return Err(invalid(
                &format!("{path} conflicts with {existing}"),
                "DLL path distinct from executable/assets, including case and file/directory prefixes",
            ));
        }
    }
    Ok(())
}

fn paths_collide(first: &str, second: &str) -> bool {
    let first = first.to_lowercase();
    let second = second.to_lowercase();
    first == second
        || first.starts_with(&format!("{second}/"))
        || second.starts_with(&format!("{first}/"))
}

fn invalid(value: &str, expected: &str) -> BuiltinReleaseError {
    BuiltinReleaseError::InvalidInput {
        value: value.into(),
        expected: expected.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::WindowsRuntimePayload;
    use std::collections::BTreeMap;

    #[test]
    fn duplicate_dll_basenames_are_rejected_independent_of_filesystem_case_rules() {
        let mut payload = WindowsRuntimePayload {
            libraries: BTreeMap::new(),
        };
        payload.insert_library("VCRUNTIME140.dll", vec![1]).unwrap();
        let error = payload
            .insert_library("vcruntime140.DLL", vec![2])
            .unwrap_err();
        assert!(error.to_string().contains("case-insensitively unique"));
        assert_eq!(payload.libraries["VCRUNTIME140.dll"], [1]);
    }
}
