use lumvise_project_indexer::{
    FilesystemProjectSource, ProjectIndexer, ScanScope, SemanticIndexProjection,
    TreeSitterProjectParser,
};
use std::{fs, path::Path};

fn scope(path: &str) -> ScanScope {
    ScanScope::Paths(vec![path.into()])
}

#[test]
fn projection_reports_parser_and_reference_coverage_without_false_edges() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("calls.rs"),
        "fn caller() { Vec::with_capacity(1); target(); } fn target() {}",
    )
    .unwrap();
    fs::write(root.path().join("other.rs"), "fn with_capacity() {}").unwrap();
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    let mut projection = SemanticIndexProjection::new(source.root(), "fixture").unwrap();
    let scan = ProjectIndexer::new(source, TreeSitterProjectParser::default())
        .prepare(ScanScope::Full)
        .unwrap();
    let batch = projection.project(&scan).unwrap();
    let file = batch
        .semantic_elements
        .iter()
        .find(|element| element.semantic_element_name == "calls.rs")
        .unwrap();
    let extraction = &file.metadata.as_ref().unwrap()["extraction"];
    assert_eq!(extraction["status"], "parsed");
    assert_eq!(extraction["resolution"]["resolved"], 1);
    assert_eq!(extraction["resolution"]["unresolved"], 1);
    assert!(batch.semantic_relationships.iter().all(|edge| {
        !edge
            .target_element_id
            .as_deref()
            .unwrap_or("")
            .contains("with_capacity")
    }));
}

#[test]
fn recursive_calls_remain_edges_and_type_uses_are_not_instantiations() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("lib.rs"),
        "struct Actual; impl Actual { fn associated() {} } fn recurse() { recurse(); }",
    )
    .unwrap();
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    let mut projection = SemanticIndexProjection::new(source.root(), "fixture").unwrap();
    let scan = ProjectIndexer::new(source, TreeSitterProjectParser::default())
        .prepare(ScanScope::Full)
        .unwrap();
    let batch = projection.project(&scan).unwrap();
    assert!(
        batch
            .semantic_relationships
            .iter()
            .any(|edge| edge.source_element_id
                == edge.target_element_id.clone().unwrap_or_default()
                && edge.relationship_label == "calls")
    );
    assert!(batch.semantic_relationships.iter().any(|edge| {
        edge.target_element_id
            .as_deref()
            .unwrap_or_default()
            .contains(":Actual:")
            && edge.relationship_label == "uses_type"
    }));
    assert!(
        batch
            .semantic_relationships
            .iter()
            .all(|edge| edge.relationship_label != "instantiates")
    );
}

#[test]
fn established_source_and_element_id_shapes_survive_line_moves_and_duplicate_names() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(
        root.path().join("src/lib.rs"),
        "fn same() {}\nfn same() {}\n",
    )
    .unwrap();
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    // A fixed logical root makes the established source identity a golden fixture.
    let mut projection =
        SemanticIndexProjection::new(Path::new("/workspace/project"), "mcp-1").unwrap();
    let mut indexer = ProjectIndexer::new(source, TreeSitterProjectParser::default());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    let batch = projection.project(&initial).unwrap();
    assert_eq!(
        batch.semantic_sources[0].semantic_source_id,
        "filesystem:3d0ae75b40dc9e45"
    );
    let ids: Vec<_> = batch
        .semantic_elements
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "filesystem:3d0ae75b40dc9e45:src:folder:src:",
            "filesystem:3d0ae75b40dc9e45:src/lib.rs:file:lib.rs:",
            "filesystem:3d0ae75b40dc9e45:src/lib.rs:function:same:",
            "filesystem:3d0ae75b40dc9e45:src/lib.rs:function:same#2:",
        ]
    );
    assert_eq!(
        batch.semantic_elements[3].metadata.as_ref().unwrap()["stable_name"],
        "same#2"
    );
    indexer.commit(initial).unwrap();
    fs::write(
        root.path().join("src/lib.rs"),
        "\n\nfn same() {}\nfn same() {}\n",
    )
    .unwrap();
    let moved = indexer.prepare(scope("src/lib.rs")).unwrap();
    let moved_batch = projection.project(&moved).unwrap();
    let moved_ids: Vec<_> = moved_batch
        .semantic_elements
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect();
    assert_eq!(moved_ids, ids[1..]);
    assert_eq!(moved_batch.semantic_elements[1].start_line, Some(3));
}

#[test]
fn nested_parents_use_declaration_ranges_and_keep_rust_method_identity_kind() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("lib.rs"),
        "struct Sample; impl Sample { fn method() { fn inner() {} } } fn sibling() {}",
    )
    .unwrap();
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    let mut projection = SemanticIndexProjection::new(source.root(), "mcp-1").unwrap();
    let scan = ProjectIndexer::new(source, TreeSitterProjectParser::default())
        .prepare(ScanScope::Full)
        .unwrap();
    let batch = projection.project(&scan).unwrap();
    let method = batch
        .semantic_elements
        .iter()
        .find(|element| element.semantic_element_name == "method")
        .unwrap();
    let inner = batch
        .semantic_elements
        .iter()
        .find(|element| element.semantic_element_name == "inner")
        .unwrap();
    let sibling = batch
        .semantic_elements
        .iter()
        .find(|element| element.semantic_element_name == "sibling")
        .unwrap();
    assert_eq!(method.semantic_element_type, "function");
    assert_eq!(
        inner.parent_element_id.as_ref(),
        Some(&method.semantic_element_id)
    );
    assert_eq!(
        sibling.parent_element_id.as_ref(),
        Some(&batch.semantic_elements[0].semantic_element_id)
    );
}

#[test]
fn changed_target_projects_complete_cached_callers_without_recomputing_their_fragments() {
    let root = tempfile::tempdir().unwrap();
    for (path, text) in [
        ("caller.rs", "fn caller() { target(); target(); }"),
        ("target.rs", "fn target() {}"),
        ("other.rs", "fn other() {}"),
    ] {
        fs::write(root.path().join(path), text).unwrap();
    }
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    let mut projection = SemanticIndexProjection::new(source.root(), "mcp-1").unwrap();
    let mut indexer = ProjectIndexer::new(source, TreeSitterProjectParser::default());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    let initial_batch = projection.project(&initial).unwrap();
    assert_eq!(initial_batch.semantic_relationships.len(), 1);
    assert_eq!(
        initial_batch.semantic_relationships[0].relationship_label,
        "calls"
    );
    assert_eq!(projection.metrics().files_projected, 3);
    indexer.commit(initial).unwrap();
    fs::write(root.path().join("target.rs"), "fn renamed() {}").unwrap();
    let changed = indexer.prepare(scope("target.rs")).unwrap();
    let changed_batch = projection.project(&changed).unwrap();
    assert_eq!(changed_batch.replace_paths, ["caller.rs", "target.rs"]);
    assert!(changed_batch.semantic_relationships.is_empty());
    assert_eq!(changed_batch.semantic_elements.len(), 4);
    assert_eq!(projection.metrics().files_projected, 4);
    drop(changed);
    let retry = indexer.prepare(scope("target.rs")).unwrap();
    assert_eq!(projection.project(&retry).unwrap(), changed_batch);
}

#[test]
fn empty_initial_snapshot_is_publishable_and_unchanged_followups_are_not() {
    let root = tempfile::tempdir().unwrap();
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    let mut projection = SemanticIndexProjection::new(source.root(), "mcp-1").unwrap();
    let mut indexer = ProjectIndexer::new(source, TreeSitterProjectParser::default());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    assert!(initial.needs_publication());
    assert!(initial.is_full_snapshot());
    let batch = projection.project(&initial).unwrap();
    assert!(
        batch.semantic_elements.is_empty()
            && batch.replace_paths.is_empty()
            && batch.removed_paths.is_empty()
    );
    indexer.commit(initial).unwrap();
    let unchanged = indexer.prepare(ScanScope::Full).unwrap();
    assert!(!unchanged.needs_publication());
    assert!(projection.project(&unchanged).is_err());
}

#[test]
fn first_full_inventory_after_partial_scan_includes_previously_cached_files() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.rs"), "fn a() {}").unwrap();
    fs::write(root.path().join("b.rs"), "fn b() {}").unwrap();
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    let mut projection = SemanticIndexProjection::new(source.root(), "mcp-1").unwrap();
    let mut indexer = ProjectIndexer::new(source, TreeSitterProjectParser::default());
    let partial = indexer.prepare(scope("a.rs")).unwrap();
    assert!(!partial.is_full_snapshot());
    indexer.commit(partial).unwrap();
    let full = indexer.prepare(ScanScope::Full).unwrap();
    assert!(full.is_full_snapshot());
    assert_eq!(full.metrics().files_read, 1);
    let batch = projection.project(&full).unwrap();
    assert!(batch.replace_paths.is_empty());
    assert_eq!(batch.semantic_elements.len(), 4);
    indexer.commit(full).unwrap();
    fs::remove_file(root.path().join("b.rs")).unwrap();
    let removed = indexer.prepare(scope("b.rs")).unwrap();
    let batch = projection.project(&removed).unwrap();
    assert_eq!(batch.removed_paths, ["b.rs"]);
    assert!(batch.semantic_elements.is_empty());
}

#[test]
fn invalid_root_or_provider_fails_before_any_projection() {
    for (root, provider) in [
        ("relative", "mcp-1"),
        ("/workspace/../project", "mcp-1"),
        ("/workspace/project", " "),
    ] {
        assert!(SemanticIndexProjection::new(Path::new(root), provider).is_err());
    }
}

#[test]
fn markdown_headings_and_text_blocks_project_as_child_elements() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("guide.md"), "# Setup\n\nBody text.\n").unwrap();
    fs::write(
        root.path().join("data.table"),
        "alpha one\nalpha two\n\nbeta\n",
    )
    .unwrap();
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    let project_root = source.root().to_path_buf();
    let scan = ProjectIndexer::new(source, TreeSitterProjectParser::default())
        .prepare(ScanScope::Full)
        .unwrap();
    let mut projection = SemanticIndexProjection::new(&project_root, "mcp-1").unwrap();
    let batch = projection.project(&scan).unwrap();
    let children: Vec<(String, String)> = batch
        .semantic_elements
        .iter()
        .filter(|element| matches!(element.semantic_element_type.as_str(), "heading" | "block"))
        .map(|element| {
            (
                element.semantic_element_type.clone(),
                element.semantic_element_name.clone(),
            )
        })
        .collect();
    assert_eq!(
        children,
        [
            ("block", "alpha one"),
            ("block", "beta"),
            ("heading", "Setup"),
        ]
        .map(|(kind, name)| (kind.into(), name.into()))
    );
}
