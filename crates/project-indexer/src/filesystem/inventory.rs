use super::{has_symlink, metadata_stamp, path_error};
use crate::source::{invalid, validate_path};
use crate::{ScanError, ScanScope, SourceEntry, SourceKind};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(super) struct ProjectInventory<'root> {
    root: &'root Path,
    policies: BTreeMap<PathBuf, (GitignoreBuilder, Gitignore)>,
    entries: BTreeMap<String, SourceEntry>,
}

impl<'root> ProjectInventory<'root> {
    pub(super) fn new(root: &'root Path) -> Self {
        Self {
            root,
            policies: BTreeMap::new(),
            entries: BTreeMap::new(),
        }
    }

    pub(super) fn collect(mut self, scope: &ScanScope) -> Result<Vec<SourceEntry>, ScanError> {
        match scope {
            ScanScope::Full => self.visit_directory(self.root)?,
            ScanScope::Paths(paths) => {
                for path in paths {
                    validate_path(path)?;
                    if has_symlink(self.root, path)? {
                        continue;
                    }
                    self.visit(&self.root.join(path))?;
                    self.include_ancestors(path)?;
                }
            }
        }
        Ok(self.entries.into_values().collect())
    }

    fn visit(&mut self, absolute: &Path) -> Result<(), ScanError> {
        let Some(entry) = self.source_entry(absolute)? else {
            return Ok(());
        };
        let directory = entry.kind == SourceKind::Directory;
        if self.entries.insert(entry.path.clone(), entry).is_some() {
            return Ok(());
        }
        if directory {
            self.visit_directory(absolute)?;
        }
        Ok(())
    }

    fn source_entry(&mut self, absolute: &Path) -> Result<Option<SourceEntry>, ScanError> {
        let metadata = match absolute.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(path_error(absolute, error)),
        };
        if metadata.is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
            return Ok(None);
        }
        let path = self.relative(absolute)?;
        if self.ignored(absolute, &path, metadata.is_dir())? {
            return Ok(None);
        }
        let kind = if metadata.is_dir() {
            SourceKind::Directory
        } else {
            SourceKind::File
        };
        Ok(Some(SourceEntry {
            path,
            kind,
            stamp: metadata_stamp(&metadata),
        }))
    }

    fn visit_directory(&mut self, directory: &Path) -> Result<(), ScanError> {
        self.ensure_policy(directory)?;
        let entries = directory
            .read_dir()
            .map_err(|error| path_error(directory, error))?;
        for entry in entries {
            let entry = entry.map_err(|error| path_error(directory, error))?;
            self.visit(&entry.path())?;
        }
        Ok(())
    }

    fn include_ancestors(&mut self, path: &str) -> Result<(), ScanError> {
        let mut parent = Path::new(path).parent();
        while let Some(relative) = parent.filter(|path| !path.as_os_str().is_empty()) {
            let absolute = self.root.join(relative);
            let relative_text = self.relative(&absolute)?;
            if absolute.is_dir() && !self.ignored(&absolute, &relative_text, true)? {
                self.entries
                    .entry(relative_text.clone())
                    .or_insert(SourceEntry {
                        path: relative_text,
                        kind: SourceKind::Directory,
                        stamp: None,
                    });
            }
            parent = relative.parent();
        }
        Ok(())
    }

    fn relative(&self, path: &Path) -> Result<String, ScanError> {
        let relative = path
            .strip_prefix(self.root)
            .map_err(|error| path_error(path, error))?;
        let relative = relative
            .to_str()
            .ok_or_else(|| invalid(path.display().to_string(), "expected UTF-8 relative path"))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        validate_path(&relative)?;
        Ok(relative)
    }

    fn ignored(
        &mut self,
        absolute: &Path,
        relative: &str,
        directory: bool,
    ) -> Result<bool, ScanError> {
        if relative.split('/').any(default_excluded) {
            return Ok(true);
        }
        let parent = absolute.parent().unwrap_or(self.root);
        self.ensure_policy(parent)?;
        Ok(self.policies[parent]
            .1
            .matched_path_or_any_parents(absolute, directory)
            .is_ignore())
    }

    fn ensure_policy(&mut self, directory: &Path) -> Result<(), ScanError> {
        if self.policies.contains_key(directory) {
            return Ok(());
        }
        let mut builder = self.inherited_policy(directory)?;
        for name in [".gitignore", ".lumignore"] {
            add_ignore_file(&mut builder, &directory.join(name))?;
        }
        let policy = builder
            .build()
            .map_err(|error| path_error(directory, error))?;
        self.policies
            .insert(directory.to_owned(), (builder, policy));
        Ok(())
    }
    fn inherited_policy(&mut self, directory: &Path) -> Result<GitignoreBuilder, ScanError> {
        if directory == self.root {
            return Ok(GitignoreBuilder::new(self.root));
        }
        let parent = directory.parent().ok_or_else(|| {
            invalid(
                directory.display().to_string(),
                "expected directory within project root",
            )
        })?;
        self.ensure_policy(parent)?;
        Ok(self.policies[parent].0.clone())
    }
}

fn add_ignore_file(builder: &mut GitignoreBuilder, path: &Path) -> Result<(), ScanError> {
    match path.try_exists() {
        Ok(false) => Ok(()),
        Err(error) => Err(path_error(path, error)),
        Ok(true) => builder
            .add(path)
            .map_or(Ok(()), |error| Err(path_error(path, error))),
    }
}

fn default_excluded(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".hg"
            | ".svn"
            | ".fastembed_cache"
            | ".lumvise"
            | ".lumwise"
            | ".scratch"
            | "target"
            | "node_modules"
            | "dist"
            | "build"
            | "__pycache__"
            | ".cache"
            | ".next"
            | ".turbo"
            | ".venv"
            | "venv"
            | "dist-electron"
            | "coverage"
    )
}
