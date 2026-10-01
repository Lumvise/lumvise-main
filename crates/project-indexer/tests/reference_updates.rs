use lumvise_project_indexer::{
    FilesystemProjectSource, PreparedProjectScan, ProjectIndexer, ReferenceTarget, ScanScope,
    TreeSitterProjectParser,
};
use std::fs;

type SourceIndexer = ProjectIndexer<FilesystemProjectSource, TreeSitterProjectParser>;

fn scope(path: &str) -> ScanScope {
    ScanScope::Paths(vec![path.into()])
}

fn indexer(root: &std::path::Path) -> SourceIndexer {
    ProjectIndexer::new(
        FilesystemProjectSource::open(root).unwrap(),
        TreeSitterProjectParser::default(),
    )
}

fn target<'a>(scan: &'a PreparedProjectScan, path: &str, name: &str) -> &'a ReferenceTarget {
    let update = scan
        .reference_updates()
        .find(|update| update.file.entry.path == path)
        .unwrap();
    &update
        .references
        .iter()
        .find(|reference| update.file.parsed.references[reference.reference_index].name == name)
        .unwrap()
        .target
}

#[test]
fn nested_functions_in_methods_resolve_as_free_definitions() {
    let root = tempfile::tempdir().unwrap();
    for (path, source, name) in [
        (
            "nested.py",
            "class Value:\n    def outer(self):\n        def inner_py():\n            return 1\n        return inner_py()\n",
            "inner_py",
        ),
        (
            "nested.ts",
            "class Widget { render() { function innerTs() { return 1; } return innerTs(); } }",
            "innerTs",
        ),
        (
            "nested.rs",
            "struct S; impl S { fn run(&self) { fn inner_rs() {} inner_rs(); } }",
            "inner_rs",
        ),
    ] {
        fs::write(root.path().join(path), source).unwrap();
        let scan = indexer(root.path()).prepare(scope(path)).unwrap();
        assert!(
            matches!(target(&scan, path, name), ReferenceTarget::Unique(site)
            if site.file.parsed.definitions[site.definition_index].name == name
                && site.file.parsed.definitions[site.definition_index].implementation_type.is_none()),
            "{path}: {:?}",
            target(&scan, path, name)
        );
    }
}

#[test]
fn csharp_local_function_calls_itself_and_its_enclosing_class_member() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("local.cs"),
        "class Value { void Helper() {} void Run() { void Local() { Helper(); } Local(); } }",
    )
    .unwrap();
    let scan = indexer(root.path()).prepare(ScanScope::Full).unwrap();
    assert!(
        matches!(target(&scan, "local.cs", "Helper"), ReferenceTarget::Unique(site)
        if site.file.parsed.definitions[site.definition_index].implementation_type.as_deref() == Some("Value"))
    );
    assert!(
        matches!(target(&scan, "local.cs", "Local"), ReferenceTarget::Unique(site)
        if site.file.parsed.definitions[site.definition_index].implementation_type.is_none())
    );
}

#[test]
fn inherited_self_and_this_calls_resolve_to_unique_members() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("base.py"),
        "class Base:\n    def shared_base(self): pass\n",
    )
    .unwrap();
    fs::write(root.path().join("child.py"), "class Child(Base):\n    def run(self):\n        def nested():\n            self.shared_base()\n        nested()\n").unwrap();
    fs::write(
        root.path().join("base.ts"),
        "class Base { sharedBase() {} }",
    )
    .unwrap();
    fs::write(
        root.path().join("child.ts"),
        "class Child extends Base { run() { const nested = () => this.sharedBase(); nested(); } }",
    )
    .unwrap();
    let scan = indexer(root.path()).prepare(ScanScope::Full).unwrap();
    for (path, name, expected) in [
        ("child.py", "shared_base", "base.py"),
        ("child.ts", "sharedBase", "base.ts"),
    ] {
        assert!(
            matches!(target(&scan, path, name), ReferenceTarget::Unique(site) if site.file.entry.path == expected),
            "{path}: {:?}",
            target(&scan, path, name)
        );
    }
}

#[test]
fn self_fallback_is_ambiguous_unless_local_or_own_member_wins() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("base.py"),
        "class Base:\n    def shared(self): pass\n",
    )
    .unwrap();
    fs::write(
        root.path().join("other.py"),
        "class Other:\n    def shared(self): pass\n",
    )
    .unwrap();
    let caller = root.path().join("child.py");
    fs::write(
        &caller,
        "class Child(Base):\n    def run(self): self.shared()\n",
    )
    .unwrap();
    let scan = indexer(root.path()).prepare(ScanScope::Full).unwrap();
    assert!(matches!(
        target(&scan, "child.py", "shared"),
        ReferenceTarget::Ambiguous { candidate_count: 2 }
    ));

    fs::write(&caller, "class Local:\n    def shared(self): pass\nclass Child(Base):\n    def run(self): self.shared()\n").unwrap();
    let scan = indexer(root.path()).prepare(ScanScope::Full).unwrap();
    assert!(
        matches!(target(&scan, "child.py", "shared"), ReferenceTarget::Unique(site)
        if site.file.parsed.definitions[site.definition_index].implementation_type.as_deref() == Some("Local"))
    );

    fs::write(&caller, "class Local:\n    def shared(self): pass\nclass Child(Base):\n    def shared(self): pass\n    def run(self): self.shared()\n").unwrap();
    let scan = indexer(root.path()).prepare(ScanScope::Full).unwrap();
    assert!(
        matches!(target(&scan, "child.py", "shared"), ReferenceTarget::Unique(site)
        if site.file.parsed.definitions[site.definition_index].implementation_type.as_deref() == Some("Child"))
    );
}

#[test]
fn qualified_and_receiver_calls_never_fall_back_to_unrelated_names() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("caller.rs"),
        "fn caller(value: Unknown) { Vec::with_capacity(1); value.extend(); Actual::make(); }",
    )
    .unwrap();
    fs::write(
        root.path().join("targets.rs"),
        "fn with_capacity() {} fn extend() {} struct Actual; impl Actual { fn make() {} }",
    )
    .unwrap();
    let scan = indexer(root.path()).prepare(ScanScope::Full).unwrap();
    assert!(matches!(
        target(&scan, "caller.rs", "with_capacity"),
        ReferenceTarget::Unresolved
    ));
    assert!(matches!(
        target(&scan, "caller.rs", "extend"),
        ReferenceTarget::Unresolved
    ));
    assert!(matches!(
        target(&scan, "caller.rs", "make"),
        ReferenceTarget::Unique(_)
    ));
}

#[test]
fn self_calls_resolve_within_their_type_and_repair_on_target_deletion() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("caller.rs"),
        "impl Actual { fn caller(&self) { self.run(); } }",
    )
    .unwrap();
    fs::write(
        root.path().join("target.rs"),
        "impl Actual { fn run(&self) {} } impl Other { fn run(&self) {} }",
    )
    .unwrap();
    let mut indexer = indexer(root.path());
    let first = indexer.prepare(ScanScope::Full).unwrap();
    assert!(
        matches!(target(&first, "caller.rs", "run"), ReferenceTarget::Unique(site)
        if site.file.parsed.definitions[site.definition_index].implementation_type.as_deref() == Some("Actual"))
    );
    indexer.commit(first).unwrap();
    fs::write(root.path().join("target.rs"), "fn run() {}").unwrap();
    let removed = indexer.prepare(scope("target.rs")).unwrap();
    assert!(matches!(
        target(&removed, "caller.rs", "run"),
        ReferenceTarget::Unresolved
    ));
}

#[test]
fn explicit_modules_and_languages_constrain_same_named_candidates() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("caller.rs"),
        "fn caller() { crate::actual::target(); shared(); }",
    )
    .unwrap();
    fs::write(
        root.path().join("actual.rs"),
        "fn target() {} fn shared() {}",
    )
    .unwrap();
    fs::write(root.path().join("other.rs"), "fn target() {}").unwrap();
    fs::write(root.path().join("other.py"), "def shared(): pass\n").unwrap();
    let scan = indexer(root.path()).prepare(ScanScope::Full).unwrap();
    for name in ["target", "shared"] {
        assert!(
            matches!(target(&scan,"caller.rs",name), ReferenceTarget::Unique(site) if site.file.entry.path == "actual.rs")
        );
    }
}

#[test]
fn dynamic_receivers_in_python_and_typescript_do_not_match_bare_functions() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("caller.py"),
        "def extend(): pass\ndef caller(value): value.extend()\n",
    )
    .unwrap();
    fs::write(
        root.path().join("caller.ts"),
        "function extend() {} function caller(value: any) { value.extend(); }",
    )
    .unwrap();
    let scan = indexer(root.path()).prepare(ScanScope::Full).unwrap();
    for path in ["caller.py", "caller.ts"] {
        assert!(matches!(
            target(&scan, path, "extend"),
            ReferenceTarget::Unresolved
        ));
    }
}

#[test]
fn python_self_call_targets_its_class_method() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("value.py"), "class Value:\n    def backward(self):\n        self._build()\n    def _build(self):\n        pass\n").unwrap();
    let scan = indexer(root.path()).prepare(ScanScope::Full).unwrap();
    assert!(
        matches!(target(&scan, "value.py", "_build"), ReferenceTarget::Unique(site)
        if site.file.parsed.definitions[site.definition_index].implementation_type.as_deref() == Some("Value"))
    );
}

#[test]
fn javascript_and_typescript_this_calls_target_class_methods() {
    let root = tempfile::tempdir().unwrap();
    for path in ["shape.js", "shape.ts"] {
        fs::write(
            root.path().join(path),
            "class Shape { area() { return this.width(); } width() { return 1; } }",
        )
        .unwrap();
    }
    fs::write(root.path().join("shape.mjs"), "const ShapeFactory = class Shape { area() { return this.width(); } width() { return 1; } };").unwrap();
    let scan = indexer(root.path()).prepare(ScanScope::Full).unwrap();
    for path in ["shape.js", "shape.ts", "shape.mjs"] {
        assert!(
            matches!(target(&scan, path, "width"), ReferenceTarget::Unique(site)
            if site.file.parsed.definitions[site.definition_index].implementation_type.as_deref() == Some("Shape")),
            "{path}: {:?}",
            target(&scan, path, "width")
        );
    }
}

#[test]
fn rust_generic_impl_calls_resolve_by_base_type() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("foo.rs"), "struct Foo<T>(T); impl<T> Foo<T> { fn new() {} fn make() { Self::new(); } } fn caller() { Foo::new(); Foo::<u8>::new(); }").unwrap();
    let scan = indexer(root.path()).prepare(ScanScope::Full).unwrap();
    let update = scan
        .reference_updates()
        .find(|update| update.file.entry.path == "foo.rs")
        .unwrap();
    let new_calls: Vec<_> = update
        .references
        .iter()
        .filter(|reference| update.file.parsed.references[reference.reference_index].name == "new")
        .collect();
    assert_eq!(new_calls.len(), 3);
    assert!(new_calls.iter().all(|reference| matches!(&reference.target, ReferenceTarget::Unique(site)
        if site.file.parsed.definitions[site.definition_index].implementation_type.as_deref() == Some("Foo"))));
}

#[test]
fn implicit_receiver_calls_resolve_only_inside_csharp_and_cpp_types() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("a.cs"),
        "class A { void Run() { Helper(); } void Helper() {} } class B { void Helper() {} }",
    )
    .unwrap();
    fs::write(
        root.path().join("a.cpp"),
        "class A { void Run() { Helper(); } void Helper() {} }; class B { void Helper() {} };",
    )
    .unwrap();
    fs::write(
        root.path().join("nested.cpp"),
        "class Outer { void Helper() {} class Inner { void Run() { Helper(); } void Helper() {} }; };",
    )
    .unwrap();
    let scan = indexer(root.path()).prepare(ScanScope::Full).unwrap();
    for path in ["a.cs", "a.cpp"] {
        assert!(
            matches!(target(&scan, path, "Helper"), ReferenceTarget::Unique(site)
            if site.file.parsed.definitions[site.definition_index].implementation_type.as_deref() == Some("A")),
            "{path}: {:?}",
            target(&scan, path, "Helper")
        );
    }
    assert!(
        matches!(target(&scan, "nested.cpp", "Helper"), ReferenceTarget::Unique(site)
        if site.file.parsed.definitions[site.definition_index].implementation_type.as_deref() == Some("Inner"))
    );
}

#[test]
fn bare_calls_do_not_target_members_in_python_javascript_typescript_or_rust() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("a.py"),
        "class A:\n    def helper(self): pass\n    def caller(self): helper()\n",
    )
    .unwrap();
    fs::write(
        root.path().join("a.js"),
        "class A { helper() {} caller() { helper(); } }",
    )
    .unwrap();
    fs::write(
        root.path().join("a.ts"),
        "class A { helper() {} caller() { helper(); } }",
    )
    .unwrap();
    fs::write(
        root.path().join("a.rs"),
        "struct A; impl A { fn helper() {} fn caller() { helper(); } }",
    )
    .unwrap();
    let scan = indexer(root.path()).prepare(ScanScope::Full).unwrap();
    for path in ["a.py", "a.js", "a.ts", "a.rs"] {
        assert!(matches!(
            target(&scan, path, "helper"),
            ReferenceTarget::Unresolved
        ));
    }
}

#[test]
fn document_keys_remain_indexed_without_competing_with_code_reference_targets() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("caller.rs"), "fn caller() { target(); }").unwrap();
    fs::write(root.path().join("target.rs"), "fn target() {}").unwrap();
    fs::write(root.path().join("settings.json"), r#"{"target": true}"#).unwrap();
    let mut indexer = indexer(root.path());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    assert!(
        matches!(target(&initial, "caller.rs", "target"), ReferenceTarget::Unique(site) if site.file.entry.path == "target.rs")
    );
    assert_eq!(
        initial
            .changed_files()
            .find(|file| file.entry.path == "settings.json")
            .unwrap()
            .parsed
            .definitions[0]
            .name,
        "target"
    );
    indexer.commit(initial).unwrap();
    fs::write(root.path().join("settings.json"), r#"{"target": false}"#).unwrap();
    let edited = indexer.prepare(scope("settings.json")).unwrap();
    assert_eq!(edited.metrics().reference_files_resolved, 1);
    indexer.commit(edited).unwrap();
    fs::remove_file(root.path().join("target.rs")).unwrap();
    let removed = indexer.prepare(scope("target.rs")).unwrap();
    assert!(matches!(
        target(&removed, "caller.rs", "target"),
        ReferenceTarget::Unresolved
    ));
}

#[test]
fn added_renamed_and_deleted_definitions_repair_unchanged_callers_without_rereading() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("caller.rs"), "fn caller() { target(); }").unwrap();
    fs::write(
        root.path().join("unrelated.rs"),
        "fn unrelated() { other(); }",
    )
    .unwrap();
    let mut indexer = indexer(root.path());
    let first = indexer.prepare(ScanScope::Full).unwrap();
    assert!(matches!(
        target(&first, "caller.rs", "target"),
        ReferenceTarget::Unresolved
    ));
    indexer.commit(first).unwrap();

    fs::write(root.path().join("target.rs"), "fn target() {}").unwrap();
    let added = indexer.prepare(scope("target.rs")).unwrap();
    assert_eq!(
        (added.metrics().files_read, added.metrics().files_parsed),
        (1, 1)
    );
    assert_eq!(
        added
            .reference_updates()
            .map(|update| update.file.entry.path.as_str())
            .collect::<Vec<_>>(),
        ["caller.rs", "target.rs"]
    );
    assert!(
        matches!(target(&added, "caller.rs", "target"), ReferenceTarget::Unique(site) if site.file.entry.path == "target.rs")
    );
    indexer.commit(added).unwrap();

    fs::write(root.path().join("target.rs"), "fn renamed() {}").unwrap();
    drop(indexer.prepare(scope("target.rs")).unwrap());
    let renamed = indexer.prepare(scope("target.rs")).unwrap();
    assert!(matches!(
        target(&renamed, "caller.rs", "target"),
        ReferenceTarget::Unresolved
    ));
    indexer.commit(renamed).unwrap();
    let stable = indexer.prepare(ScanScope::Full).unwrap();
    assert_eq!(stable.metrics().reference_files_resolved, 0);

    fs::write(root.path().join("target.rs"), "fn target() {}").unwrap();
    let restored = indexer.prepare(scope("target.rs")).unwrap();
    indexer.commit(restored).unwrap();
    fs::remove_file(root.path().join("target.rs")).unwrap();
    let removed = indexer.prepare(scope("target.rs")).unwrap();
    assert_eq!(removed.metrics().files_read, 0);
    assert_eq!(removed.metrics().reference_files_resolved, 1);
    assert!(matches!(
        target(&removed, "caller.rs", "target"),
        ReferenceTarget::Unresolved
    ));
}

#[test]
fn duplicate_names_are_explicit_and_unique_local_definition_takes_precedence() {
    let root = tempfile::tempdir().unwrap();
    for path in ["a.rs", "b.rs"] {
        fs::write(root.path().join(path), "fn target() {}").unwrap();
    }
    fs::write(root.path().join("caller.rs"), "fn caller() { target(); }").unwrap();
    let mut indexer = indexer(root.path());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    assert!(matches!(
        target(&initial, "caller.rs", "target"),
        ReferenceTarget::Ambiguous { candidate_count: 2 }
    ));
    indexer.commit(initial).unwrap();
    fs::write(
        root.path().join("caller.rs"),
        "fn target() {} fn caller() { target(); }",
    )
    .unwrap();
    let local = indexer.prepare(scope("caller.rs")).unwrap();
    assert!(
        matches!(target(&local, "caller.rs", "target"), ReferenceTarget::Unique(site) if site.file.entry.path == "caller.rs")
    );
    indexer.commit(local).unwrap();
    fs::write(
        root.path().join("caller.rs"),
        "fn target() {} fn caller() { target(); } fn target() {}",
    )
    .unwrap();
    let ambiguous_local = indexer.prepare(scope("caller.rs")).unwrap();
    assert!(matches!(
        target(&ambiguous_local, "caller.rs", "target"),
        ReferenceTarget::Ambiguous { candidate_count: 2 }
    ));
}

#[test]
fn unchanged_callers_receive_new_target_ranges_and_correct_nested_owners() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("target.rs"), "fn target() {}").unwrap();
    fs::write(
        root.path().join("caller.rs"),
        "fn outer() { fn inner() { target(); } target(); }",
    )
    .unwrap();
    let mut indexer = indexer(root.path());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    let caller = initial
        .reference_updates()
        .find(|update| update.file.entry.path == "caller.rs")
        .unwrap();
    let owners: Vec<_> = caller
        .references
        .iter()
        .map(|reference| {
            caller.file.parsed.definitions[reference.owner_definition.unwrap()]
                .name
                .as_str()
        })
        .collect();
    assert_eq!(owners, ["inner", "outer"]);
    indexer.commit(initial).unwrap();
    fs::write(
        root.path().join("target.rs"),
        "fn added() {}\n\nfn target() { added(); }",
    )
    .unwrap();
    let changed = indexer.prepare(scope("target.rs")).unwrap();
    let ReferenceTarget::Unique(site) = target(&changed, "caller.rs", "target") else {
        panic!("expected unique target")
    };
    let definition = &site.file.parsed.definitions[site.definition_index];
    assert_eq!(
        (definition.name.as_str(), definition.start_line),
        ("target", 3)
    );
    assert_eq!(changed.metrics().files_parsed, 1);
}

#[test]
fn changed_callers_remove_old_reverse_names_and_publish_empty_reference_lists() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("caller.rs"), "fn caller() { target(); }").unwrap();
    let mut indexer = indexer(root.path());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    indexer.commit(initial).unwrap();
    fs::write(root.path().join("caller.rs"), "fn caller() {}").unwrap();
    let cleared = indexer.prepare(scope("caller.rs")).unwrap();
    assert!(
        cleared
            .reference_updates()
            .next()
            .unwrap()
            .references
            .is_empty()
    );
    indexer.commit(cleared).unwrap();
    fs::write(root.path().join("target.rs"), "fn target() {}").unwrap();
    let added = indexer.prepare(scope("target.rs")).unwrap();
    assert_eq!(added.metrics().reference_files_resolved, 1);
    assert_eq!(
        added.reference_updates().next().unwrap().file.entry.path,
        "target.rs"
    );
}

#[test]
fn constructor_calls_can_resolve_to_class_definitions() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("caller.py"),
        "def caller():\n    Target()\n",
    )
    .unwrap();
    fs::write(root.path().join("target.py"), "class Target:\n    pass\n").unwrap();
    let scan = indexer(root.path()).prepare(ScanScope::Full).unwrap();
    assert!(
        matches!(target(&scan, "caller.py", "Target"), ReferenceTarget::Unique(site) if site.file.parsed.definitions[site.definition_index].kind == "class")
    );
}
