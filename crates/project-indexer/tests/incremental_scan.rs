use lumvise_project_indexer::{
    FileRead, FileStamp, IndexedDefinition, ParsedFile, ProjectFileParser, ProjectIndexer,
    ProjectSource, ScanError, ScanScope, SourceEntry, SourceKind,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

#[derive(Default)]
struct SourceState {
    files: BTreeMap<String, (u8, Vec<u8>)>,
    reads: Vec<String>,
    requested_scopes: Vec<ScanScope>,
    fail_inventory: bool,
    fail_read: Option<String>,
}
#[derive(Clone, Default)]
struct FakeProjectSource(Rc<RefCell<SourceState>>);
impl FakeProjectSource {
    fn put(&self, path: &str, stamp: u8, text: &str) {
        self.0
            .borrow_mut()
            .files
            .insert(path.into(), (stamp, text.as_bytes().to_vec()));
    }
    fn remove(&self, path: &str) {
        self.0.borrow_mut().files.remove(path);
    }
    fn reset_reads(&self) {
        self.0.borrow_mut().reads.clear();
    }
}
impl ProjectSource for FakeProjectSource {
    fn inventory(&self, scope: &ScanScope) -> Result<Vec<SourceEntry>, ScanError> {
        let mut state = self.0.borrow_mut();
        state.requested_scopes.push(scope.clone());
        if state.fail_inventory {
            return Err(ScanError {
                path: "src".into(),
                reason: "expected readable directory".into(),
            });
        }
        Ok(state
            .files
            .iter()
            .filter(|(path, _)| match scope {
                ScanScope::Full => true,
                ScanScope::Paths(paths) => paths
                    .iter()
                    .any(|prefix| *path == prefix || path.starts_with(&format!("{prefix}/"))),
            })
            .map(|(path, (stamp, _))| SourceEntry {
                path: path.clone(),
                kind: SourceKind::File,
                stamp: Some(FileStamp(vec![*stamp])),
            })
            .collect())
    }
    fn read_file(&self, path: &str) -> Result<FileRead, ScanError> {
        let mut state = self.0.borrow_mut();
        state.reads.push(path.into());
        if state.fail_read.as_deref() == Some(path) {
            return Err(ScanError {
                path: path.into(),
                reason: "expected readable file".into(),
            });
        }
        let (stamp, bytes) = &state.files[path];
        Ok(FileRead {
            bytes: bytes.clone(),
            stamp: Some(FileStamp(vec![*stamp])),
        })
    }
}

#[test]
fn resilient_updates_keep_failed_content_and_retry_it_after_healthy_changes_commit() {
    let source = FakeProjectSource::default();
    source.put("bad.rs", 1, "old_bad");
    source.put("good.rs", 1, "old_good");
    let mut indexer = ProjectIndexer::new(source.clone(), FakeLanguageParser::default());
    let initial = indexer
        .prepare(ScanScope::Paths(vec!["bad.rs".into(), "good.rs".into()]))
        .unwrap();
    indexer.commit(initial).unwrap();
    source.put("bad.rs", 2, "parse-error");
    source.put("good.rs", 2, "new_good");
    let update = indexer
        .prepare_resilient(ScanScope::Full, |_| Ok(()))
        .unwrap();
    assert_eq!(update.failures().len(), 1);
    assert_eq!(update.failures()[0].path, "bad.rs");
    assert_eq!(update.failures()[0].reason, "expected valid source");
    assert_eq!(
        update
            .changed_files()
            .map(|file| file.entry.path.as_str())
            .collect::<Vec<_>>(),
        ["bad.rs", "good.rs"]
    );
    assert_eq!(
        update.changed_files().next().unwrap().parsed.definitions[0].name,
        "old_bad"
    );
    indexer.commit(update).unwrap();
    source.put("bad.rs", 1, "old_bad");
    let retained = indexer.prepare(ScanScope::Full).unwrap();
    assert_eq!(retained.metrics().files_read, 0);
    assert_eq!(retained.changed_files().count(), 0);
    source.put("bad.rs", 3, "repaired");
    let retry = indexer
        .prepare_resilient(ScanScope::Full, |_| Ok(()))
        .unwrap();
    assert!(retry.failures().is_empty());
    assert_eq!(
        retry
            .changed_files()
            .map(|file| file.entry.path.as_str())
            .collect::<Vec<_>>(),
        ["bad.rs"]
    );
}

#[test]
fn resilient_initial_scan_reports_read_and_parse_failures_without_inventing_empty_files() {
    let source = FakeProjectSource::default();
    source.put("bad.rs", 1, "parse-error");
    source.put("good.rs", 1, "healthy");
    source.put("unreadable.rs", 1, "unreadable");
    source.0.borrow_mut().fail_read = Some("unreadable.rs".into());
    let mut indexer = ProjectIndexer::new(source, FakeLanguageParser::default());
    let scan = indexer
        .prepare_resilient(ScanScope::Full, |_| Ok(()))
        .unwrap();
    assert_eq!(scan.failures().len(), 2);
    let reasons: BTreeMap<_, _> = scan
        .failures()
        .iter()
        .map(|failure| (failure.path.as_str(), failure.reason.as_str()))
        .collect();
    assert_eq!(
        reasons,
        BTreeMap::from([
            ("bad.rs", "expected valid source"),
            ("unreadable.rs", "expected readable file"),
        ])
    );
    assert_eq!(
        scan.changed_files()
            .map(|file| file.entry.path.as_str())
            .collect::<Vec<_>>(),
        ["good.rs"]
    );
}

#[test]
fn acknowledged_unparsed_paths_still_publish_later_deletions() {
    for scope in [ScanScope::Full, ScanScope::Paths(vec!["bad.rs".into()])] {
        let source = FakeProjectSource::default();
        source.put("bad.rs", 1, "parse-error");
        source.put("good.rs", 1, "healthy");
        let mut indexer = ProjectIndexer::new(source.clone(), FakeLanguageParser::default());
        assert!(!indexer.has_inventory_baseline());
        let initial = indexer
            .prepare_resilient(ScanScope::Full, |_| Ok(()))
            .unwrap();
        indexer.commit(initial).unwrap();
        assert!(indexer.has_inventory_baseline());
        source.remove("bad.rs");
        let deletion = indexer.prepare_resilient(scope, |_| Ok(())).unwrap();
        assert_eq!(deletion.removed_paths().collect::<Vec<_>>(), ["bad.rs"]);
        assert!(deletion.needs_publication());
        indexer.commit(deletion).unwrap();
        assert!(
            !indexer
                .prepare(ScanScope::Full)
                .unwrap()
                .needs_publication()
        );
    }
}

#[test]
fn resilient_inventory_failure_still_aborts_instead_of_deleting_unseen_paths() {
    let source = FakeProjectSource::default();
    source.0.borrow_mut().fail_inventory = true;
    let mut indexer = ProjectIndexer::new(source, FakeLanguageParser::default());
    assert!(
        indexer
            .prepare_resilient(ScanScope::Full, |_| Ok(()))
            .is_err()
    );
}

#[derive(Clone, Default)]
struct FakeLanguageParser(Rc<RefCell<Vec<String>>>);
impl ProjectFileParser for FakeLanguageParser {
    fn parse(&mut self, path: &str, bytes: &[u8]) -> Result<ParsedFile, ScanError> {
        self.0.borrow_mut().push(path.into());
        if bytes == b"parse-error" {
            return Err(ScanError {
                path: path.into(),
                reason: "expected valid source".into(),
            });
        }
        Ok(ParsedFile {
            definitions: vec![IndexedDefinition {
                kind: "function".into(),
                name: String::from_utf8_lossy(bytes).into(),
                start_line: 1,
                end_line: 1,
                span: Default::default(),
                implementation_type: None,
            }],
            references: vec![],
            ..ParsedFile::default()
        })
    }
}
fn paths(paths: &[&str]) -> ScanScope {
    ScanScope::Paths(paths.iter().map(|path| (*path).to_owned()).collect())
}

#[test]
fn explicit_change_reads_and_parses_one_file_out_of_ten_thousand() {
    let source = FakeProjectSource::default();
    for index in 0..10_000 {
        source.put(&format!("src/{index}.rs"), 1, "original");
    }
    let parser = FakeLanguageParser::default();
    let mut indexer = ProjectIndexer::new(source.clone(), parser.clone());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    assert_eq!(initial.metrics().files_read, 10_000);
    assert_eq!(initial.changed_files().count(), 10_000);
    indexer.commit(initial).unwrap();
    source.reset_reads();
    parser.0.borrow_mut().clear();
    source.put("src/421.rs", 2, "updated");
    let changed = indexer.prepare(paths(&["src/421.rs"])).unwrap();
    assert_eq!(changed.metrics().entries_inspected, 1);
    assert_eq!(changed.metrics().files_read, 1);
    assert_eq!(changed.metrics().files_parsed, 1);
    assert_eq!(source.0.borrow().reads, ["src/421.rs"]);
    assert_eq!(*parser.0.borrow(), ["src/421.rs"]);
    assert_eq!(
        changed.changed_files().next().unwrap().parsed.definitions[0].name,
        "updated"
    );
    indexer.commit(changed).unwrap();
    let unchanged = indexer.prepare(ScanScope::Full).unwrap();
    assert!(!unchanged.needs_publication());
    assert_eq!(unchanged.metrics().files_read, 0);
    assert_eq!(unchanged.metrics().files_parsed, 0);
}

#[test]
fn metadata_only_change_reads_once_and_reuses_exact_content_parse() {
    let source = FakeProjectSource::default();
    source.put("a.rs", 1, "original");
    let mut indexer = ProjectIndexer::new(source.clone(), FakeLanguageParser::default());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    indexer.commit(initial).unwrap();
    source.put("a.rs", 2, "original");
    let touched = indexer.prepare(paths(&["a.rs"])).unwrap();
    assert_eq!(
        (touched.metrics().files_read, touched.metrics().files_parsed),
        (1, 0)
    );
    assert!(!touched.needs_publication());
    indexer.commit(touched).unwrap();
    assert_eq!(
        indexer
            .prepare(paths(&["a.rs"]))
            .unwrap()
            .metrics()
            .files_read,
        0
    );
}

#[test]
fn invalidated_paths_force_full_inventory_and_fresh_digest_reads_without_reparsing_unchanged_bytes()
{
    let source = FakeProjectSource::default();
    source.put("src/a.rs", 1, "original_a");
    source.put("src/b.rs", 1, "original_b");
    let parser = FakeLanguageParser::default();
    let mut indexer = ProjectIndexer::new(source.clone(), parser.clone());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    indexer.commit(initial).unwrap();
    source.reset_reads();
    parser.0.borrow_mut().clear();

    let invalidated = BTreeSet::from(["src/a.rs".to_owned()]);
    let recovered = indexer
        .prepare_resilient_invalidating(paths(&["src/a.rs"]), &invalidated, |_| Ok(()))
        .unwrap();

    assert_eq!(
        source.0.borrow().requested_scopes.last(),
        Some(&ScanScope::Full)
    );
    assert_eq!(source.0.borrow().reads, ["src/a.rs"]);
    assert_eq!(recovered.metrics().files_read, 1);
    assert_eq!(recovered.metrics().files_parsed, 0);
    assert!(parser.0.borrow().is_empty());
    assert_eq!(
        recovered
            .changed_files()
            .map(|file| file.entry.path.as_str())
            .collect::<Vec<_>>(),
        ["src/a.rs"]
    );
    assert_eq!(
        recovered.changed_files().next().unwrap().parsed.definitions[0].name,
        "original_a"
    );
}

#[test]
fn invalidated_digest_mismatch_parses_fresh_bytes_when_inventory_stamp_is_unchanged() {
    let source = FakeProjectSource::default();
    source.put("src/a.rs", 7, "accepted_bytes");
    let parser = FakeLanguageParser::default();
    let mut indexer = ProjectIndexer::new(source.clone(), parser.clone());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    indexer.commit(initial).unwrap();
    source.reset_reads();
    parser.0.borrow_mut().clear();
    source.put("src/a.rs", 7, "fresh_bytes");

    let invalidated = BTreeSet::from(["src/a.rs".to_owned()]);
    let recovered = indexer
        .prepare_resilient_invalidating(ScanScope::Full, &invalidated, |_| Ok(()))
        .unwrap();

    assert_eq!(source.0.borrow().reads, ["src/a.rs"]);
    assert_eq!(*parser.0.borrow(), ["src/a.rs"]);
    assert_eq!(recovered.metrics().files_parsed, 1);
    let emitted = recovered.changed_files().collect::<Vec<_>>();
    assert_eq!(emitted.len(), 1);
    let file = emitted[0];
    assert_eq!(file.entry.path, "src/a.rs");
    assert_eq!(file.parsed.definitions[0].name, "fresh_bytes");
}

#[test]
fn invalidated_parse_failure_retains_accepted_content_and_does_not_emit_the_failed_path() {
    let source = FakeProjectSource::default();
    source.put("src/a.rs", 1, "accepted_a");
    source.put("src/b.rs", 1, "accepted_b");
    let mut indexer = ProjectIndexer::new(source.clone(), FakeLanguageParser::default());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    indexer.commit(initial).unwrap();
    source.put("src/a.rs", 2, "parse-error");

    let invalidated = BTreeSet::from(["src/a.rs".to_owned()]);
    let recovered = indexer
        .prepare_resilient_invalidating(ScanScope::Full, &invalidated, |_| Ok(()))
        .unwrap();

    assert_eq!(recovered.failures().len(), 1);
    assert_eq!(recovered.failures()[0].path, "src/a.rs");
    assert!(recovered.changed_files().next().is_none());
    indexer.commit(recovered).unwrap();
    source.put("src/a.rs", 1, "accepted_a");
    let unchanged = indexer.prepare(ScanScope::Full).unwrap();
    assert!(!unchanged.needs_publication());
    assert_eq!(unchanged.metrics().files_read, 0);
}

#[test]
fn invalidated_read_failure_retains_the_accepted_path_for_the_next_scan() {
    let source = FakeProjectSource::default();
    source.put("src/a.rs", 1, "accepted_a");
    source.put("src/b.rs", 1, "accepted_b");
    let mut indexer = ProjectIndexer::new(source.clone(), FakeLanguageParser::default());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    indexer.commit(initial).unwrap();
    source.reset_reads();
    source.0.borrow_mut().fail_read = Some("src/a.rs".into());

    let invalidated = BTreeSet::from(["src/a.rs".to_owned()]);
    let failed = indexer
        .prepare_resilient_invalidating(ScanScope::Full, &invalidated, |_| Ok(()))
        .unwrap();

    assert_eq!(source.0.borrow().reads, ["src/a.rs"]);
    assert_eq!(failed.failures().len(), 1);
    assert_eq!(failed.failures()[0].path, "src/a.rs");
    assert!(failed.changed_files().next().is_none());
    indexer.commit(failed).unwrap();
    source.0.borrow_mut().fail_read = None;

    let accepted = indexer.prepare(ScanScope::Full).unwrap();
    assert!(!accepted.needs_publication());
    assert_eq!(accepted.metrics().files_read, 0);
}

#[test]
fn invalidated_path_absent_from_complete_inventory_is_published_as_removed() {
    let source = FakeProjectSource::default();
    source.put("src/a.rs", 1, "accepted_a");
    source.put("src/b.rs", 1, "accepted_b");
    let mut indexer = ProjectIndexer::new(source.clone(), FakeLanguageParser::default());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    indexer.commit(initial).unwrap();
    source.remove("src/a.rs");

    let invalidated = BTreeSet::from(["src/a.rs".to_owned()]);
    let removed = indexer
        .prepare_resilient_invalidating(paths(&["src/a.rs"]), &invalidated, |_| Ok(()))
        .unwrap();

    assert_eq!(
        source.0.borrow().requested_scopes.last(),
        Some(&ScanScope::Full)
    );
    assert_eq!(removed.removed_paths().collect::<Vec<_>>(), ["src/a.rs"]);
    assert!(removed.changed_files().next().is_none());
    assert!(removed.needs_publication());
    indexer.commit(removed).unwrap();
    assert!(
        !indexer
            .prepare(ScanScope::Full)
            .unwrap()
            .needs_publication()
    );
}

#[test]
fn failed_publication_and_parser_failure_leave_changes_retryable() {
    let source = FakeProjectSource::default();
    source.put("a.rs", 1, "original");
    let mut indexer = ProjectIndexer::new(source.clone(), FakeLanguageParser::default());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    indexer.commit(initial).unwrap();
    source.put("a.rs", 2, "updated");
    drop(indexer.prepare(paths(&["a.rs"])).unwrap());
    assert!(
        indexer
            .prepare(paths(&["a.rs"]))
            .unwrap()
            .needs_publication()
    );
    source.put("a.rs", 3, "parse-error");
    assert!(
        indexer
            .prepare(paths(&["a.rs"]))
            .err()
            .unwrap()
            .to_string()
            .contains("a.rs")
    );
    source.put("a.rs", 4, "retry");
    let retry = indexer.prepare(paths(&["a.rs"])).unwrap();
    assert_eq!(
        retry.changed_files().next().unwrap().parsed.definitions[0].name,
        "retry"
    );
}

#[test]
fn deletion_stays_in_partition_and_failed_inventory_never_deletes() {
    let source = FakeProjectSource::default();
    for path in [
        "src/a/one.rs",
        "src/a/two.rs",
        "src/a-other.rs",
        "src/abc/keep.rs",
    ] {
        source.put(path, 1, "source");
    }
    let mut indexer = ProjectIndexer::new(source.clone(), FakeLanguageParser::default());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    indexer.commit(initial).unwrap();
    source.remove("src/a/one.rs");
    source.remove("src/a/two.rs");
    source.0.borrow_mut().fail_inventory = true;
    assert!(indexer.prepare(paths(&["src/a"])).is_err());
    source.0.borrow_mut().fail_inventory = false;
    let deleted = indexer.prepare(paths(&["src/a"])).unwrap();
    assert_eq!(
        deleted.removed_paths().collect::<Vec<_>>(),
        ["src/a/one.rs", "src/a/two.rs"]
    );
    indexer.commit(deleted).unwrap();
    assert!(
        !indexer
            .prepare(ScanScope::Full)
            .unwrap()
            .needs_publication()
    );
}

#[test]
fn stale_preparation_cannot_overwrite_a_newer_acknowledged_scan() {
    let source = FakeProjectSource::default();
    source.put("a.rs", 1, "original");
    let mut indexer = ProjectIndexer::new(source.clone(), FakeLanguageParser::default());
    let stale = indexer.prepare(ScanScope::Full).unwrap();
    source.put("a.rs", 2, "updated");
    let current = indexer.prepare(ScanScope::Full).unwrap();
    indexer.commit(current).unwrap();
    assert!(
        indexer
            .commit(stale)
            .err()
            .unwrap()
            .to_string()
            .contains("expected current scan generation 1")
    );
    assert!(
        !indexer
            .prepare(ScanScope::Full)
            .unwrap()
            .needs_publication()
    );
}

#[test]
fn invalid_scopes_fail_before_any_source_access() {
    let source = FakeProjectSource::default();
    let mut indexer = ProjectIndexer::new(source.clone(), FakeLanguageParser::default());
    for scope in [
        paths(&[]),
        paths(&["../outside"]),
        paths(&["/absolute"]),
        paths(&["src//file"]),
    ] {
        assert!(indexer.prepare(scope).is_err());
    }
    assert!(source.0.borrow().requested_scopes.is_empty());
}

#[test]
fn another_project_indexer_cannot_acknowledge_the_preparation() {
    let source = FakeProjectSource::default();
    source.put("a.rs", 1, "original");
    let mut first = ProjectIndexer::new(source.clone(), FakeLanguageParser::default());
    let scan = first.prepare(ScanScope::Full).unwrap();
    let mut second = ProjectIndexer::new(source, FakeLanguageParser::default());
    assert!(
        second
            .commit(scan)
            .err()
            .unwrap()
            .to_string()
            .contains("this project indexer")
    );
    assert!(second.prepare(ScanScope::Full).unwrap().needs_publication());
}
