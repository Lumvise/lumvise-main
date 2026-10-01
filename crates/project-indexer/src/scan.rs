use crate::ReferenceFileUpdate;
use crate::references::ReferenceIndex;
use crate::source::{invalid, validate_path};
use crate::{
    ParsedFile, ProjectFileParser, ProjectSource, ScanError, ScanScope, SourceEntry, SourceKind,
};
mod reads;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Work counters from the actual scan. Example: an unchanged scan has zero reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanMetrics {
    /// Accepted inventory entries inspected.
    pub entries_inspected: usize,
    /// File bodies read once for freshness and parsing.
    pub files_read: usize,
    /// Changed file bodies passed to the language parser.
    pub files_parsed: usize,
    /// Bytes read from source files.
    pub bytes_read: usize,
    /// Files whose references were reconsidered, including cached unchanged callers.
    pub reference_files_resolved: usize,
}

/// One cached file or directory selected for publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScannedFile {
    /// Inventory metadata corresponding to the parsed content.
    pub entry: SourceEntry,
    /// Exact content digest; directories have an all-zero digest.
    pub digest: [u8; 32],
    /// Existing semantic matching format, computed from the same stable file read.
    pub fingerprint: Option<crate::SourceFingerprint>,
    /// Number of bytes in the stable source read; zero for directories.
    pub byte_len: u64,
    /// Reusable language structure; no database records are cached here.
    pub parsed: Arc<ParsedFile>,
}

/// Prepared work remains separate from acknowledged state. Drop it after failed
/// publication; the next prepare will still include the unpublished changes.
pub struct PreparedProjectScan {
    owner: Arc<()>,
    generation: u64,
    replacements: BTreeMap<String, Arc<ScannedFile>>,
    changed: BTreeSet<String>,
    removed: BTreeSet<String>,
    metrics: ScanMetrics,
    reference_updates: BTreeMap<String, ReferenceFileUpdate>,
    full_snapshot: bool,
}

impl PreparedProjectScan {
    fn new(
        owner: &Arc<()>,
        generation: u64,
        removed: BTreeSet<String>,
        inspected: usize,
        full_snapshot: bool,
    ) -> Self {
        Self {
            owner: Arc::clone(owner),
            generation,
            full_snapshot,
            removed,
            replacements: BTreeMap::new(),
            reference_updates: BTreeMap::new(),
            changed: BTreeSet::new(),
            metrics: ScanMetrics {
                entries_inspected: inspected,
                ..ScanMetrics::default()
            },
        }
    }

    fn retain(&mut self, file: Arc<ScannedFile>, changed: bool) {
        if changed || self.full_snapshot {
            self.changed.insert(file.entry.path.clone());
        }
        self.replacements.insert(file.entry.path.clone(), file);
    }

    /// Whether this preparation establishes the complete project baseline, including
    /// an empty project. Example: publish it with the existing full-snapshot operation.
    pub fn is_full_snapshot(&self) -> bool {
        self.full_snapshot
    }

    /// Returns changed records only. Example: publish these before `commit(scan)`.
    pub fn changed_files(&self) -> impl Iterator<Item = &ScannedFile> {
        self.changed
            .iter()
            .map(|path| self.replacements[path].as_ref())
    }
    /// Returns deletions within the requested scope, ready for partition publication.
    pub fn removed_paths(&self) -> impl Iterator<Item = &str> {
        self.removed.iter().map(String::as_str)
    }
    /// Complete reference replacements for changed files and affected cached callers.
    /// Example: publish these alongside `changed_files` before acknowledging the scan.
    pub fn reference_updates(&self) -> impl Iterator<Item = &ReferenceFileUpdate> {
        self.reference_updates.values()
    }
    /// Returns measured I/O and parser work. Example: assert `files_read == 1`.
    pub fn metrics(&self) -> &ScanMetrics {
        &self.metrics
    }
    /// Indicates whether semantic publication is necessary; metadata-only changes
    /// may still be acknowledged to avoid rereading unchanged content next time.
    pub fn needs_publication(&self) -> bool {
        self.full_snapshot || !self.changed.is_empty() || !self.removed.is_empty()
    }
}

/// One project's acknowledged scan state. Source and parser are injected, with no
/// process globals or database access. Example: `ProjectIndexer::new(source, parser)`.
pub struct ProjectIndexer<S, P> {
    owner: Arc<()>,
    source: S,
    parser: P,
    generation: u64,
    files: BTreeMap<String, Arc<ScannedFile>>,
    references: ReferenceIndex,
    snapshot_acknowledged: bool,
}

impl<S: ProjectSource, P: ProjectFileParser> ProjectIndexer<S, P> {
    /// Creates empty acknowledged state for one project.
    pub fn new(source: S, parser: P) -> Self {
        Self {
            owner: Arc::new(()),
            source,
            parser,
            generation: 0,
            files: BTreeMap::new(),
            references: ReferenceIndex::default(),
            snapshot_acknowledged: false,
        }
    }

    /// Prepares a delta without acknowledging it. Explicit paths avoid inventory
    /// of unrelated files; callers publish and then call `commit` on success.
    pub fn prepare(&mut self, scope: ScanScope) -> Result<PreparedProjectScan, ScanError> {
        validate_scope(&scope)?;
        let scope = expand_ignore_changes(scope);
        let entries = self.source.inventory(&scope)?;
        let present = validate_inventory(&entries, &scope)?;
        let removed = self.removed_in_scope(&scope, &present);
        let full_snapshot = matches!(scope, ScanScope::Full) && !self.snapshot_acknowledged;
        let mut scan = PreparedProjectScan::new(
            &self.owner,
            self.generation,
            removed,
            entries.len(),
            full_snapshot,
        );
        for entries in entries.chunks(self.parser.batch_size().get()) {
            self.prepare_batch(entries, &mut scan)?;
        }
        scan.reference_updates = self.references.prepare(
            &self.files,
            &scan.replacements,
            &scan.changed,
            &scan.removed,
        );
        scan.metrics.reference_files_resolved = scan.reference_updates.len();
        Ok(scan)
    }

    /// Advances only acknowledged source state. A stale preparation cannot overwrite
    /// a newer publication. Example: call after the semantic ingest succeeds.
    pub fn commit(&mut self, scan: PreparedProjectScan) -> Result<(), ScanError> {
        self.validate_preparation(&scan)?;
        self.generation = self.generation.checked_add(1).ok_or_else(|| {
            invalid(
                self.generation.to_string(),
                "expected incrementable scan generation",
            )
        })?;
        self.commit_references(&scan);
        self.snapshot_acknowledged |= scan.full_snapshot;
        for path in scan.removed {
            self.files.remove(&path);
        }
        self.files.extend(scan.replacements);
        Ok(())
    }

    fn commit_references(&mut self, scan: &PreparedProjectScan) {
        for path in scan.changed.union(&scan.removed) {
            if let Some(file) = self.files.get(path) {
                self.references.remove(file);
            }
        }
        for path in &scan.changed {
            self.references.insert(&scan.replacements[path]);
        }
    }

    fn validate_preparation(&self, scan: &PreparedProjectScan) -> Result<(), ScanError> {
        if !Arc::ptr_eq(&scan.owner, &self.owner) {
            return Err(invalid(
                "foreign scan",
                "expected preparation from this project indexer",
            ));
        }
        if scan.generation != self.generation {
            return Err(invalid(
                scan.generation.to_string(),
                format!("expected current scan generation {}", self.generation),
            ));
        }
        Ok(())
    }

    fn removed_in_scope(&self, scope: &ScanScope, present: &BTreeSet<String>) -> BTreeSet<String> {
        let candidates: BTreeSet<&String> = match scope {
            ScanScope::Full => self.files.keys().collect(),
            ScanScope::Paths(paths) => paths
                .iter()
                .flat_map(|path| self.cached_paths_under(path))
                .collect(),
        };
        candidates
            .into_iter()
            .filter(|path| !present.contains(*path))
            .cloned()
            .collect()
    }

    fn cached_paths_under<'a>(&'a self, path: &str) -> impl Iterator<Item = &'a String> {
        let prefix = format!("{path}/");
        self.files
            .get_key_value(path)
            .map(|(key, _)| key)
            .into_iter()
            .chain(
                self.files
                    .range(prefix.clone()..)
                    .take_while(move |(key, _)| key.starts_with(&prefix))
                    .map(|(key, _)| key),
            )
    }
}

fn freshness_matches(old: &ScannedFile, entry: &SourceEntry) -> bool {
    old.entry.kind == entry.kind
        && (entry.kind == SourceKind::Directory
            || entry
                .stamp
                .as_ref()
                .is_some_and(|stamp| old.entry.stamp.as_ref() == Some(stamp)))
}

fn validate_scope(scope: &ScanScope) -> Result<(), ScanError> {
    if let ScanScope::Paths(paths) = scope {
        if paths.is_empty() {
            return Err(invalid(
                "[]",
                "expected one or more changed paths, or Full scope",
            ));
        }
        for path in paths {
            validate_path(path)?;
        }
    }
    Ok(())
}

fn validate_inventory(
    entries: &[SourceEntry],
    scope: &ScanScope,
) -> Result<BTreeSet<String>, ScanError> {
    let mut paths = BTreeSet::new();
    for entry in entries {
        validate_path(&entry.path)?;
        if !paths.insert(entry.path.clone()) {
            return Err(invalid(
                &entry.path,
                "expected one inventory entry per path",
            ));
        }
        if !entry_allowed(entry, scope) {
            return Err(invalid(
                &entry.path,
                "expected entry within requested paths or a directory ancestor",
            ));
        }
    }
    Ok(paths)
}

fn entry_allowed(entry: &SourceEntry, scope: &ScanScope) -> bool {
    let ScanScope::Paths(paths) = scope else {
        return true;
    };
    paths.iter().any(|path| {
        under(&entry.path, path)
            || (entry.kind == SourceKind::Directory && under(path, &entry.path))
    })
}

fn under(path: &str, parent: &str) -> bool {
    path == parent
        || path
            .strip_prefix(parent)
            .is_some_and(|tail| tail.starts_with('/'))
}

fn expand_ignore_changes(scope: ScanScope) -> ScanScope {
    let ScanScope::Paths(paths) = scope else {
        return scope;
    };
    let mut expanded = BTreeSet::new();
    for path in paths {
        if matches!(path.rsplit('/').next(), Some(".gitignore" | ".lumignore")) {
            let Some((parent, _)) = path.rsplit_once('/') else {
                return ScanScope::Full;
            };
            expanded.insert(parent.to_owned());
        } else {
            expanded.insert(path);
        }
    }
    ScanScope::Paths(expanded.into_iter().collect())
}
