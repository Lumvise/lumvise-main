use lumvise_project_indexer::{
    FilesystemProjectSource, ProjectIndexer, ScanScope, SemanticIndexProjection,
    TreeSitterProjectParser, html_section_texts,
};
use std::collections::BTreeSet;
use std::fs;

/// Several sections on one line: line ranges cannot tell them apart, fingerprints can.
const ONE_LINE_PAGE: &str = "<html><body><h2>Alpha</h2><p>first body words</p><h3>Beta</h3><p>second body <a href=\"/wiki/Other\">linked</a> words</p><h2>Gamma</h2><p>third &amp; last</p></body></html>\n";

fn indexed_section_fingerprints(page: &str) -> BTreeSet<String> {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("wiki")).unwrap();
    fs::write(root.path().join("wiki/Page.html"), page).unwrap();
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    let mut projection = SemanticIndexProjection::new(source.root(), "fixture").unwrap();
    let mut indexer = ProjectIndexer::new(source, TreeSitterProjectParser::default());
    let scan = indexer.prepare(ScanScope::Full).unwrap();
    let batch = projection.project(&scan).unwrap();
    indexer.commit(scan).unwrap();
    batch
        .semantic_elements
        .iter()
        .filter(|element| element.semantic_element_type == "html_section")
        .map(|element| {
            element
                .content_fingerprint
                .clone()
                .expect("section fingerprint")
        })
        .collect()
}

#[test]
fn section_texts_match_indexed_fingerprints_for_sections_on_one_line() {
    let indexed = indexed_section_fingerprints(ONE_LINE_PAGE);
    let sections = html_section_texts(ONE_LINE_PAGE).unwrap();

    let extracted: BTreeSet<String> = sections
        .iter()
        .map(|section| section.content_fingerprint.clone())
        .collect();
    assert_eq!(indexed.len(), 3);
    assert_eq!(extracted, indexed);
}

#[test]
fn section_text_is_the_visible_words_the_fingerprint_covers() {
    let sections = html_section_texts(ONE_LINE_PAGE).unwrap();
    let texts: Vec<&str> = sections
        .iter()
        .map(|section| section.text.as_str())
        .collect();
    assert_eq!(
        texts,
        [
            "Alpha first body words",
            "Beta second body linked words",
            "Gamma third & last",
        ]
    );
}

#[test]
fn section_texts_are_empty_for_a_page_without_headings() {
    let sections = html_section_texts("<html><body><p>No heading here.</p></body></html>").unwrap();
    assert!(sections.is_empty());
}
