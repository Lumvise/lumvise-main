use lumvise_project_indexer::{
    FilesystemProjectSource, ParsedFile, ProjectFileParser, ProjectIndexer, ProjectSource,
    ScanError, ScanScope, SourceKind,
};
use std::fs;

struct FakeByteParser;
impl ProjectFileParser for FakeByteParser {
    fn parse(&mut self, _: &str, _: &[u8]) -> Result<ParsedFile, ScanError> {
        Ok(ParsedFile::default())
    }
}
fn paths(paths: &[&str]) -> ScanScope {
    ScanScope::Paths(paths.iter().map(|path| (*path).to_owned()).collect())
}

#[test]
fn full_and_scoped_inventory_honor_the_same_nested_project_ignores() {
    let root = tempfile::tempdir().unwrap();
    for directory in ["src", "ignored", "generated", "target"] {
        fs::create_dir(root.path().join(directory)).unwrap();
    }
    fs::write(
        root.path().join(".gitignore"),
        "/generated/\n/src/root-only.rs\n",
    )
    .unwrap();
    fs::write(root.path().join(".lumignore"), "ignored/\n").unwrap();
    fs::write(root.path().join("src/.lumignore"), "skip.rs\n").unwrap();
    for path in [
        "src/a.rs",
        "src/skip.rs",
        "src/root-only.rs",
        "ignored/x.rs",
        "generated/x.rs",
        "target/x.rs",
    ] {
        fs::write(root.path().join(path), "fn a() {}\n").unwrap();
    }
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    assert_eq!(source.root(), root.path().canonicalize().unwrap());
    let full = source.inventory(&ScanScope::Full).unwrap();
    assert!(full.iter().any(|entry| entry.path == "src/a.rs"));
    assert!(!full.iter().any(|entry| entry.path.ends_with("x.rs")
        || entry.path.ends_with("skip.rs")
        || entry.path.ends_with("root-only.rs")));
    let scoped = source.inventory(&paths(&["src"])).unwrap();
    assert_eq!(
        full.into_iter()
            .filter(|entry| entry.path == "src" || entry.path.starts_with("src/"))
            .collect::<Vec<_>>(),
        scoped
    );
    for ignored in [
        "src/skip.rs",
        "src/root-only.rs",
        "ignored/x.rs",
        "generated/x.rs",
        "target/x.rs",
    ] {
        assert!(
            !source
                .inventory(&paths(&[ignored]))
                .unwrap()
                .iter()
                .any(|entry| entry.kind == SourceKind::File)
        );
    }
}

#[test]
fn scoped_inventory_supplies_ancestors_without_reading_unrelated_files() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("src/nested")).unwrap();
    fs::write(root.path().join("src/nested/a.rs"), "one").unwrap();
    fs::write(root.path().join("src/b.rs"), "two").unwrap();
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    let selected = source.inventory(&paths(&["src/nested/a.rs"])).unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>(),
        ["src", "src/nested", "src/nested/a.rs"]
    );
    assert_eq!(source.read_file("src/nested/a.rs").unwrap().bytes, b"one");
    assert!(source.read_file("src").is_err());
    assert!(source.read_file("../outside").is_err());
}

#[cfg(unix)]
#[test]
fn same_length_and_restored_mtime_change_still_invalidates_source_cache() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("a.rs");
    fs::write(&file, "one").unwrap();
    let modified = file.metadata().unwrap().modified().unwrap();
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    let mut indexer = ProjectIndexer::new(source, FakeByteParser);
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    let digest = initial.changed_files().next().unwrap().digest;
    indexer.commit(initial).unwrap();
    assert_eq!(
        indexer
            .prepare(ScanScope::Full)
            .unwrap()
            .metrics()
            .files_read,
        0
    );
    fs::write(&file, "two").unwrap();
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let changed = indexer.prepare(paths(&["a.rs"])).unwrap();
    assert_eq!(changed.metrics().files_read, 1);
    assert_ne!(changed.changed_files().next().unwrap().digest, digest);
}

#[cfg(unix)]
#[test]
fn symlink_files_and_directory_ancestors_never_escape_the_project() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("external.rs"), "external").unwrap();
    symlink(outside.path(), root.path().join("linked")).unwrap();
    symlink(
        outside.path().join("external.rs"),
        root.path().join("linked.rs"),
    )
    .unwrap();
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    assert!(source.inventory(&ScanScope::Full).unwrap().is_empty());
    assert!(
        source
            .inventory(&paths(&["linked/external.rs"]))
            .unwrap()
            .is_empty()
    );
    assert!(source.read_file("linked/external.rs").is_err());
    assert!(source.read_file("linked.rs").is_err());
}

#[test]
fn ignore_file_change_reconciles_the_affected_subtree() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(root.path().join("src/a.rs"), "one").unwrap();
    fs::write(root.path().join("src/.lumignore"), "").unwrap();
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    let mut indexer = ProjectIndexer::new(source, FakeByteParser);
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    indexer.commit(initial).unwrap();
    fs::write(root.path().join("src/.lumignore"), "a.rs\n").unwrap();
    let ignored = indexer.prepare(paths(&["src/.lumignore"])).unwrap();
    assert_eq!(ignored.removed_paths().collect::<Vec<_>>(), ["src/a.rs"]);
    indexer.commit(ignored).unwrap();
    fs::write(root.path().join(".gitignore"), "src/\n").unwrap();
    let ignored = indexer.prepare(paths(&[".gitignore"])).unwrap();
    assert_eq!(
        ignored.removed_paths().collect::<Vec<_>>(),
        ["src", "src/.lumignore"]
    );
}

#[test]
#[ignore = "filesystem scan evidence; run in release with --ignored --nocapture; parser is a named fake"]
fn filesystem_work_for_ten_thousand_files_and_one_changed_path() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..10_000 {
        fs::write(root.path().join(format!("{index}.rs")), "fn example() {}\n").unwrap();
    }
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    let mut indexer = ProjectIndexer::new(source, FakeByteParser);
    for (label, scope) in [
        ("initial", ScanScope::Full),
        ("unchanged", ScanScope::Full),
        ("one_change", paths(&["42.rs"])),
    ] {
        if label == "one_change" {
            fs::write(root.path().join("42.rs"), "fn changed() {}\n").unwrap();
        }
        let started = std::time::Instant::now();
        let scan = indexer.prepare(scope).unwrap();
        let elapsed_us = started.elapsed().as_micros();
        let work = scan.metrics();
        eprintln!(
            "{{\"workload\":\"filesystem_10k_fake_parser\",\"case\":\"{label}\",\"elapsed_us\":{elapsed_us},\"reads\":{},\"parses\":{}}}",
            work.files_read, work.files_parsed
        );
        if label == "one_change" {
            assert_eq!((work.files_read, work.files_parsed), (1, 1));
        }
        indexer.commit(scan).unwrap();
    }
}
