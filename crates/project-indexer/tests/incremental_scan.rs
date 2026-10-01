use lumvise_project_indexer::{
    FileRead, FileStamp, IndexedDefinition, ParsedFile, ProjectFileParser, ProjectIndexer,
    ProjectSource, ScanError, ScanScope, SourceEntry, SourceKind,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

#[derive(Default)]
struct SourceState {
    files: BTreeMap<String, (u8, Vec<u8>)>,
    reads: Vec<String>,
    requested_scopes: Vec<ScanScope>,
    fail_inventory: bool,
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
        let (stamp, bytes) = &state.files[path];
        Ok(FileRead {
            bytes: bytes.clone(),
            stamp: Some(FileStamp(vec![*stamp])),
        })
    }
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
