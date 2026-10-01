use lumvise_project_indexer::{
    FilesystemProjectSource, ProjectFileParser, ProjectIndexer, ScanScope, SemanticIndexProjection,
    TreeSitterProjectParser, convert_document,
};
use std::fs;

#[test]
fn documents_convert_before_semantic_extraction_and_native_grammars_keep_precedence() {
    let mut parser = TreeSitterProjectParser::default();
    for (path, bytes) in [
        ("report.docx", include_bytes!("fixtures/documents/report.docx").as_slice()),
        ("table.csv", b"Name,Value\nCoast,12\n".as_slice()),
        ("report.html", b"<h1>Field report</h1><p>Coastal observations.</p><h2>Sensors</h2><p>Calibrate monthly.</p>".as_slice()),
        ("notes.ipynb", br##"{"cells":[{"cell_type":"markdown","source":["# Field report\n","Coastal observations."]}]}"##.as_slice()),
    ] {
        let parsed = parser.parse(path, bytes).unwrap();
        assert_eq!(parsed.document.as_ref().unwrap().converter, "anytomd", "{path}");
        assert!(!parsed.definitions.is_empty(), "{path}");
        assert!(parsed.definitions.iter().all(|definition| definition.kind == "markdown_section"));
    }
    for (path, bytes) in [
        ("code.rs", b"fn sensor() {}".as_slice()),
        ("schema.json", b"{\"sensor\": 2}".as_slice()),
        ("notes.md", b"# Field report".as_slice()),
    ] {
        let parsed = parser.parse(path, bytes).unwrap();
        assert!(parsed.document.is_none());
        assert!(!parsed.definitions.is_empty());
    }
}

#[test]
fn pdf_passages_keep_original_page_and_exact_text_in_published_selectors() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("report.pdf"),
        include_bytes!("fixtures/documents/report.pdf"),
    )
    .unwrap();
    let mut indexer = ProjectIndexer::new(
        FilesystemProjectSource::open(root.path()).unwrap(),
        TreeSitterProjectParser::default(),
    );
    let mut projection = SemanticIndexProjection::new(root.path(), "test").unwrap();
    let scan = indexer.prepare(ScanScope::Full).unwrap();
    let batch = projection.project(&scan).unwrap();
    let passage = batch
        .semantic_elements
        .iter()
        .find(|element| element.semantic_element_name.starts_with("Coastal sensors"))
        .unwrap();
    let metadata = passage.metadata.as_ref().unwrap();
    assert_eq!(metadata["anchor_selector"]["page"], 2);
    assert_eq!(
        metadata["anchor_selector"]["exact"],
        "Coastal sensors require monthly calibration."
    );
    assert_eq!(
        metadata["markdown_alias"]["coordinate_space"],
        "converted_markdown"
    );
    let markdown = convert_document(
        "report.pdf",
        include_bytes!("fixtures/documents/report.pdf"),
    )
    .unwrap()
    .unwrap()
    .markdown;
    assert!(
        markdown
            .lines()
            .nth(passage.start_line.unwrap() as usize - 1)
            .unwrap()
            .contains("Coastal")
    );
}

#[test]
fn conversion_failure_is_visible_without_aborting_unrelated_files_and_updates_replace_sections() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("bad.docx"), b"invalid ZIP").unwrap();
    fs::write(
        root.path().join("report.html"),
        b"<h1>Before</h1><p>Old paragraph.</p>",
    )
    .unwrap();
    let mut indexer = ProjectIndexer::new(
        FilesystemProjectSource::open(root.path()).unwrap(),
        TreeSitterProjectParser::default(),
    );
    let mut projection = SemanticIndexProjection::new(root.path(), "test").unwrap();
    let scan = indexer.prepare(ScanScope::Full).unwrap();
    let batch = projection.project(&scan).unwrap();
    let bad = batch
        .semantic_elements
        .iter()
        .find(|element| element.path == "bad.docx")
        .unwrap();
    assert_eq!(
        bad.metadata.as_ref().unwrap()["markdown_alias"]["status"],
        "unavailable"
    );
    indexer.commit(scan).unwrap();
    fs::write(
        root.path().join("report.html"),
        b"<h1>After</h1><p>New paragraph.</p>",
    )
    .unwrap();
    let changed = projection
        .project(
            &indexer
                .prepare(ScanScope::Paths(vec!["report.html".into()]))
                .unwrap(),
        )
        .unwrap();
    assert_eq!(changed.replace_paths, ["report.html"]);
    assert!(
        changed
            .semantic_elements
            .iter()
            .any(|element| element.semantic_element_name == "After")
    );
    assert!(
        !changed
            .semantic_elements
            .iter()
            .any(|element| element.semantic_element_name == "Before")
    );
}

#[test]
fn images_are_not_converted_and_unreadable_pdf_reports_no_fake_text() {
    assert!(convert_document("image.png", &[0, 255]).unwrap().is_none());
    assert!(
        convert_document("broken.pdf", b"%PDF-invalid")
            .unwrap_err()
            .to_string()
            .contains("readable PDF")
    );
}

#[test]
fn pdf_graphics_paths_do_not_prevent_text_conversion() {
    let mut pdf =
        lopdf::Document::load_mem(include_bytes!("fixtures/documents/report.pdf")).unwrap();
    let page = *pdf.get_pages().get(&1).unwrap();
    pdf.add_page_contents(page, b"0 0 10 10 v".to_vec())
        .unwrap();
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();
    let converted = convert_document("illustrated.pdf", &bytes)
        .unwrap()
        .unwrap();
    assert!(
        converted
            .markdown
            .contains("Ocean measurements increased steadily.")
    );
    assert_eq!(converted.provenance.pages.len(), 2);
}
