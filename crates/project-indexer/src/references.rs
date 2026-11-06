//! Derived name indexes repair cached callers after definition changes. Nothing here
//! is authoritative persistence; acknowledged source files are the only input state.
mod links;
mod resolution;

pub use links::{LinkTarget, ResolvedLink};

use crate::{ScannedFile, SourceKind};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// A definition in the same prepared source revision as its referring file.
/// Example: `target.file.parsed.definitions[target.definition_index]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionSite {
    /// Shared target content and declarations, with no extra source body copy.
    pub file: Arc<ScannedFile>,
    /// Index into this file's parsed definitions, never a persistent graph ID.
    pub definition_index: usize,
}

/// Explicit name-resolution outcome. Ambiguity never picks an arbitrary project file.
/// These are name candidates, not compiler type/import resolution: a class may be
/// called as a constructor, and a parameter may hold a callable value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReferenceTarget {
    /// No compatible declaration in the project; may be an external dependency.
    Unresolved,
    /// One compatible local declaration, otherwise one unique project declaration.
    Unique(DefinitionSite),
    /// Several compatible declarations; imports/type analysis may disambiguate later.
    Ambiguous {
        /// Number of compatible declarations, without a quadratic candidate payload.
        candidate_count: usize,
    },
}

/// One reference resolved from a cached syntax result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedReference {
    /// Index into ReferenceFileUpdate::file.parsed.references.
    pub reference_index: usize,
    /// Innermost declaration containing this occurrence, or None for file scope.
    pub owner_definition: Option<usize>,
    /// Resolution is explicit even for missing and ambiguous names.
    pub target: ReferenceTarget,
}

/// Complete replacement of one source file's reference results. An empty list
/// clears previously published references. Example: publish each prepared update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferenceFileUpdate {
    /// May be an unchanged cached caller affected by another file's declarations.
    pub file: Arc<ScannedFile>,
    /// All references in the file, not only newly resolved occurrences.
    pub references: Vec<ResolvedReference>,
    /// All hyperlinks in the file, resolved against the same inventory as `references`.
    pub links: Vec<ResolvedLink>,
}

type SourceFiles = BTreeMap<String, Arc<ScannedFile>>;
type DefinitionNames = BTreeMap<String, BTreeMap<(String, usize), DefinitionSite>>;

#[derive(Default)]
pub(crate) struct ReferenceIndex {
    definitions: DefinitionNames,
    referrers: BTreeMap<String, BTreeSet<String>>,
    /// Candidate page path of each same-site link, mapped to the pages linking to it.
    page_referrers: BTreeMap<String, BTreeSet<String>>,
}

impl ReferenceIndex {
    pub(crate) fn prepare(
        &self,
        files: &SourceFiles,
        replacements: &SourceFiles,
        changed: &BTreeSet<String>,
        removed: &BTreeSet<String>,
    ) -> BTreeMap<String, ReferenceFileUpdate> {
        let touched: BTreeSet<_> = changed.union(removed).cloned().collect();
        let added = definition_names(changed.iter().filter_map(|path| replacements.get(path)));
        let mut affected = changed.clone();
        let names: BTreeSet<_> = changed_definition_names(files, &touched)
            .chain(added.keys().cloned())
            .collect();
        for name in names {
            affected.extend(self.referrers.get(&name).into_iter().flatten().cloned());
        }
        // Only pages that appeared or vanished change what a link resolves to.
        let appeared_or_removed = removed.iter().chain(
            replacements
                .keys()
                .filter(|path| !files.contains_key(*path)),
        );
        for path in appeared_or_removed {
            affected.extend(self.page_referrers.get(path).into_iter().flatten().cloned());
        }
        let present = |path: &str| page_present(files, replacements, removed, path);
        let mut view = resolution::ResolutionView::new(&self.definitions, &added, &touched);
        affected
            .into_iter()
            .filter(|path| !removed.contains(path))
            .filter_map(|path| {
                let file = replacements.get(&path).or_else(|| files.get(&path))?;
                (file.entry.kind == SourceKind::File).then(|| {
                    let mut update = view.resolve_file(file);
                    update.links = links::resolve_file(file, present);
                    (path, update)
                })
            })
            .collect()
    }

    pub(crate) fn remove(&mut self, file: &ScannedFile) {
        let path = &file.entry.path;
        for (index, definition) in reference_definitions(file) {
            if let Some(sites) = self.definitions.get_mut(&definition.name) {
                sites.remove(&(path.clone(), index));
                if sites.is_empty() {
                    self.definitions.remove(&definition.name);
                }
            }
        }
        for reference in &file.parsed.references {
            if let Some(paths) = self.referrers.get_mut(&reference.name) {
                paths.remove(path);
                if paths.is_empty() {
                    self.referrers.remove(&reference.name);
                }
            }
        }
        for page in links::candidate_pages(file) {
            if let Some(paths) = self.page_referrers.get_mut(&page) {
                paths.remove(path);
                if paths.is_empty() {
                    self.page_referrers.remove(&page);
                }
            }
        }
    }

    pub(crate) fn insert(&mut self, file: &Arc<ScannedFile>) {
        insert_definitions(&mut self.definitions, file);
        for reference in &file.parsed.references {
            self.referrers
                .entry(reference.name.clone())
                .or_default()
                .insert(file.entry.path.clone());
        }
        for page in links::candidate_pages(file) {
            self.page_referrers
                .entry(page)
                .or_default()
                .insert(file.entry.path.clone());
        }
    }
}

/// Whether a project file exists in the inventory after this preparation.
fn page_present(
    files: &SourceFiles,
    replacements: &SourceFiles,
    removed: &BTreeSet<String>,
    path: &str,
) -> bool {
    let file = match replacements.get(path) {
        Some(file) => Some(file),
        None if removed.contains(path) => None,
        None => files.get(path),
    };
    file.is_some_and(|file| file.entry.kind == SourceKind::File)
}

fn definition_names<'a>(files: impl Iterator<Item = &'a Arc<ScannedFile>>) -> DefinitionNames {
    let mut names = DefinitionNames::new();
    for file in files {
        insert_definitions(&mut names, file);
    }
    names
}

fn changed_definition_names<'a>(
    files: &'a SourceFiles,
    touched: &'a BTreeSet<String>,
) -> impl Iterator<Item = String> + 'a {
    touched
        .iter()
        .filter_map(|path| files.get(path))
        .flat_map(|file| reference_definitions(file).map(|(_, definition)| definition.name.clone()))
}

fn insert_definitions(names: &mut DefinitionNames, file: &Arc<ScannedFile>) {
    for (index, definition) in reference_definitions(file) {
        names.entry(definition.name.clone()).or_default().insert(
            (file.entry.path.clone(), index),
            DefinitionSite {
                file: Arc::clone(file),
                definition_index: index,
            },
        );
    }
}

fn reference_definitions(
    file: &ScannedFile,
) -> impl Iterator<Item = (usize, &crate::IndexedDefinition)> {
    // Document keys and page structure are searchable elements, not declarations in a
    // code namespace: a `<header>` chrome element must not resolve a code `header`.
    let code_namespace = !matches!(
        &file.parsed.status,
        crate::ParseStatus::Parsed { language, .. } if language == "json" || language == "html"
    );
    file.parsed
        .definitions
        .iter()
        .enumerate()
        .filter(move |_| code_namespace)
}
