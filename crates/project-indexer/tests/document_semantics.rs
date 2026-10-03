use lumvise_project_indexer::{
    DocumentConversionOptions, DocumentConverter, FilesystemProjectSource,
    ParallelTreeSitterProjectParser, ParseStatus, ProjectFileParser, ProjectIndexer, ScanScope,
    SemanticIndexProjection, TreeSitterProjectParser,
};
use std::{fs, sync::Arc};

const DOCX: &[u8] = include_bytes!("fixtures/documents/illustrated.docx");
const PPTX: &[u8] = include_bytes!("fixtures/documents/illustrated.pptx");
const XLSX: &[u8] = include_bytes!("fixtures/documents/illustrated.xlsx");

#[test]
fn illustrated_office_documents_project_hierarchy_and_image_anchors_without_payloads() {
    for (path, bytes) in [
        ("report.docx", DOCX),
        ("slides.pptx", PPTX),
        ("chart.xlsx", XLSX),
    ] {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(path), bytes).unwrap();
        let source = FilesystemProjectSource::open(root.path()).unwrap();
        let mut projection = SemanticIndexProjection::new(source.root(), "documents").unwrap();
        let scan = ProjectIndexer::new(source, TreeSitterProjectParser::default())
            .prepare(ScanScope::Full)
            .unwrap();
        let batch = projection.project(&scan).unwrap();
        let file = batch
            .semantic_elements
            .iter()
            .find(|element| element.path == path && element.semantic_element_type == "file")
            .unwrap();
        let metadata = file.metadata.as_ref().unwrap();
        let markdown = metadata["markdown_alias"]["content"].as_str().unwrap();
        assert!(
            markdown.contains("Coastal observations"),
            "{path}: {markdown}"
        );
        assert!(
            markdown.contains("Region") && markdown.contains("Coast"),
            "{path}: {markdown}"
        );
        assert!(markdown.contains("!["), "{path}: {markdown}");

        let images = metadata["markdown_alias"]["images"].as_array().unwrap();
        assert!(!images.is_empty(), "{path}");
        assert!(images.iter().all(|image| image.get("bytes").is_none()));
        let image_elements: Vec<_> = batch
            .semantic_elements
            .iter()
            .filter(|element| {
                element.path == path
                    && element.metadata.as_ref().is_some_and(|metadata| {
                        metadata["anchor_selector"]["kind"] == "document_image"
                    })
            })
            .collect();
        assert_eq!(image_elements.len(), images.len(), "{path}");
        for element in image_elements {
            let selector = &element.metadata.as_ref().unwrap()["anchor_selector"];
            assert!(
                images.iter().any(|image| {
                    image["reference"] == selector["reference"] && image["page"] == selector["page"]
                }),
                "{path}: unresolved image anchor {selector}"
            );
        }
        let published = serde_json::to_string(&batch).unwrap().to_ascii_lowercase();
        assert!(!published.contains("base64"), "{path}");
        assert!(!published.contains("data:image/"), "{path}");
    }
}

#[test]
fn word_heading_hierarchy_and_unchanged_scan_are_stable() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("report.docx"), DOCX).unwrap();
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    let mut projection = SemanticIndexProjection::new(source.root(), "documents").unwrap();
    let mut indexer = ProjectIndexer::new(source, TreeSitterProjectParser::default());
    let first = indexer.prepare(ScanScope::Full).unwrap();
    let batch = projection.project(&first).unwrap();
    let parent = batch
        .semantic_elements
        .iter()
        .find(|element| {
            element.path == "report.docx" && element.semantic_element_name == "Field report"
        })
        .unwrap();
    let child = batch
        .semantic_elements
        .iter()
        .find(|element| {
            element.path == "report.docx" && element.semantic_element_name == "Coastal sensors"
        })
        .unwrap();
    assert_eq!(
        child.parent_element_id.as_ref(),
        Some(&parent.semantic_element_id)
    );
    indexer.commit(first).unwrap();

    let unchanged = indexer.prepare(ScanScope::Full).unwrap();
    assert!(!unchanged.needs_publication());
    assert!(unchanged.changed_files().next().is_none());
}

#[test]
fn parallel_parser_shares_explicitly_disabled_document_enhancement_options() {
    let bytes = include_bytes!("fixtures/documents/report.pdf");
    let converter = Arc::new(DocumentConverter::new(DocumentConversionOptions {
        enhancement_enabled: false,
    }));
    let converted = converter.convert("report.pdf", bytes).unwrap().unwrap();
    let blocks = lumvise_project_indexer::BlockSettings::default();
    let mut sequential =
        TreeSitterProjectParser::default().with_document_converter(Arc::clone(&converter));
    let sequential_file = sequential.parse("report.pdf", bytes).unwrap();
    let mut parallel = ParallelTreeSitterProjectParser::new_with_document_converter(
        2,
        blocks,
        Arc::clone(&converter),
    )
    .unwrap();
    let parallel_file = parallel.parse("report.pdf", bytes).unwrap();

    assert_eq!(converted.provenance.enhancement, "disabled");
    assert_eq!(
        sequential_file.source.as_deref(),
        Some(converted.markdown.as_str())
    );
    assert_eq!(parallel_file.source, sequential_file.source);
    assert_eq!(parallel_file.status, sequential_file.status);
    assert!(matches!(parallel_file.status, ParseStatus::Parsed { .. }));
}
