use lumvise_contracts::{IndexBatchRequest, SemanticElementUpsert, SemanticRelationshipUpsert};
use lumvise_project_indexer::{
    FilesystemProjectSource, ProjectIndexer, ScanScope, SemanticIndexProjection,
    TreeSitterProjectParser,
};
use std::collections::BTreeMap;
use std::fs;

const TOPIC: &str = r##"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<meta name="lumvise:source-url" content="https://wiki.example.org/wiki/Topic">
<title>Topic</title>
<style>.hatnote { color: gray; }</style>
<script>
document.write("<a href='/wiki/Fake'>fake</a>");
</script>
</head>
<body class="skin">
<p class="hatnote">Redirect note: <a href="/wiki/Existing">top link</a></p>
<nav class="menu"><a href="/wiki/Main_Page">Main page</a><h2>Menu heading</h2></nav>
<header><a href="/">Site</a></header>
<article>
<h1 class="title">Topic</h1>
<p>Lead text with <a href="/wiki/Existing">link to existing</a> and <a href="https://other.example.net/info?x=1&amp;y=2">external</a>.</p>
<h2 id="History">History</h2>
<p>Early history &amp; notes. See <a href="/wiki/Stub">stub page</a>.</p>
<h3>Early life</h3>
<p>Born somewhere. <a href="/wiki/Missing_Page">missing</a></p>
<figure><img src="x.png" alt=""><figcaption>Photo from <a href="/wiki/Existing#Section">archive</a></figcaption></figure>
<h3>Later life</h3>
<ul><li><a href="/wiki/Existing.html">Existing html</a></li><li><a href="mailto:x@example.org">mail</a></li></ul>
<p>Relative: <a href="Existing">relative Existing</a></p>
<h2>Legacy</h2>
<p>Done. <a href="#History">jump</a> <a href="javascript:void(0)">script link</a> <a href="tel:+15550100">call</a></p>
</article>
<aside><a href="/wiki/Existing">aside link</a></aside>
<footer><a href="https://wiki.example.org/wiki/Terms">Terms</a></footer>
</body>
</html>
"##;

const EXISTING: &str = r#"<!DOCTYPE html>
<html><head><title>Existing</title></head>
<body><h1>Existing</h1><p>Plain page.</p></body></html>
"#;

const STUB: &str = r#"<!DOCTYPE html>
<html><head><meta name="lumvise:stub" content="true"><title>Stub</title></head>
<body><p>Placeholder.</p></body></html>
"#;

/// Scans and acknowledges a project the way a successful publication does.
struct Project {
    root: tempfile::TempDir,
    indexer: ProjectIndexer<FilesystemProjectSource, TreeSitterProjectParser>,
    projection: SemanticIndexProjection,
}

impl Project {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = FilesystemProjectSource::open(root.path()).unwrap();
        let projection = SemanticIndexProjection::new(source.root(), "fixture").unwrap();
        Self {
            root,
            indexer: ProjectIndexer::new(source, TreeSitterProjectParser::default()),
            projection,
        }
    }

    fn write(&self, path: &str, contents: &str) {
        let full = self.root.path().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, contents).unwrap();
    }

    fn remove(&self, path: &str) {
        fs::remove_file(self.root.path().join(path)).unwrap();
    }

    fn publish(&mut self, scope: ScanScope) -> IndexBatchRequest {
        let scan = self.indexer.prepare(scope).unwrap();
        let batch = self.projection.project(&scan).unwrap();
        self.indexer.commit(scan).unwrap();
        batch
    }
}

fn find<'a>(
    batch: &'a IndexBatchRequest,
    path: &str,
    kind: &str,
    name: &str,
) -> &'a SemanticElementUpsert {
    batch
        .semantic_elements
        .iter()
        .find(|element| {
            element.path == path
                && element.semantic_element_type == kind
                && element.semantic_element_name == name
        })
        .unwrap_or_else(|| panic!("expected {kind} `{name}` in {path}"))
}

fn file_of<'a>(batch: &'a IndexBatchRequest, path: &str) -> &'a SemanticElementUpsert {
    find(batch, path, "file", path.rsplit('/').next().unwrap())
}

/// The page's `links_to` relationship whose anchor text is `label`.
fn link<'a>(
    batch: &'a IndexBatchRequest,
    owner: &SemanticElementUpsert,
    label: &str,
) -> &'a SemanticRelationshipUpsert {
    batch
        .semantic_relationships
        .iter()
        .find(|relationship| {
            relationship.source_element_id == owner.semantic_element_id
                && relationship.relationship_label == "links_to"
                && relationship.target_label.as_deref() == Some(label)
        })
        .unwrap_or_else(|| {
            panic!(
                "expected link `{label}` from `{}`",
                owner.semantic_element_name
            )
        })
}

fn links_from<'a>(
    batch: &'a IndexBatchRequest,
    owner: &SemanticElementUpsert,
) -> Vec<&'a SemanticRelationshipUpsert> {
    batch
        .semantic_relationships
        .iter()
        .filter(|relationship| {
            relationship.source_element_id == owner.semantic_element_id
                && relationship.relationship_label == "links_to"
        })
        .collect()
}

fn assert_page(relationship: &SemanticRelationshipUpsert, target: &SemanticElementUpsert) {
    assert_eq!(
        relationship.target_element_id.as_deref(),
        Some(target.semantic_element_id.as_str())
    );
}

/// The link's target is the external element named by `url`, owned by the linking page.
fn assert_external(
    batch: &IndexBatchRequest,
    relationship: &SemanticRelationshipUpsert,
    page: &str,
    url: &str,
) {
    let target = find(batch, page, "html_external_link", url);
    assert_eq!(
        relationship.target_element_id.as_deref(),
        Some(target.semantic_element_id.as_str())
    );
}

/// No relationship from `owner` carries anchor text `label`.
fn assert_no_link(batch: &IndexBatchRequest, owner: &SemanticElementUpsert, label: &str) {
    assert!(
        !links_from(batch, owner)
            .iter()
            .any(|relationship| relationship.target_label.as_deref() == Some(label)),
        "same-site link `{label}` to an absent page must not produce an edge"
    );
}

fn section_fingerprints(batch: &IndexBatchRequest, path: &str) -> BTreeMap<String, String> {
    batch
        .semantic_elements
        .iter()
        .filter(|element| element.path == path && element.semantic_element_type == "html_section")
        .map(|element| {
            (
                element.semantic_element_name.clone(),
                element
                    .content_fingerprint
                    .clone()
                    .expect("section fingerprint"),
            )
        })
        .collect()
}

fn line_of(page: &str, needle: &str) -> Option<i64> {
    page.lines()
        .position(|line| line.contains(needle))
        .map(|index| index as i64 + 1)
}

fn topic_project() -> Project {
    let project = Project::new();
    project.write("wiki/Topic.html", TOPIC);
    project.write("wiki/Existing.html", EXISTING);
    project.write("wiki/Stub.html", STUB);
    project
}

#[test]
fn html_headings_nest_as_sections_and_chrome_is_marked() {
    let mut project = topic_project();
    let batch = project.publish(ScanScope::Full);
    let page = "wiki/Topic.html";
    let topic = find(&batch, page, "html_section", "Topic");
    let history = find(&batch, page, "html_section", "History");
    let early = find(&batch, page, "html_section", "Early life");
    let later = find(&batch, page, "html_section", "Later life");
    let legacy = find(&batch, page, "html_section", "Legacy");

    let sections = batch
        .semantic_elements
        .iter()
        .filter(|element| element.path == page && element.semantic_element_type == "html_section")
        .count();
    assert_eq!(
        sections, 5,
        "the heading inside nav must not become a section"
    );
    assert_eq!(
        history.parent_element_id.as_deref(),
        Some(topic.semantic_element_id.as_str())
    );
    assert_eq!(
        early.parent_element_id.as_deref(),
        Some(history.semantic_element_id.as_str())
    );
    assert_eq!(
        later.parent_element_id.as_deref(),
        Some(history.semantic_element_id.as_str())
    );
    assert_eq!(
        legacy.parent_element_id.as_deref(),
        Some(topic.semantic_element_id.as_str())
    );
    assert_eq!(history.start_line, line_of(TOPIC, "<h2 id=\"History\">"));
    assert_eq!(early.start_line, line_of(TOPIC, "<h3>Early life</h3>"));
    // A section runs up to the line before the next heading of the same or higher level.
    assert_eq!(later.end_line, legacy.start_line.map(|line| line - 1));

    for name in ["nav", "header", "aside", "footer"] {
        let chrome = find(&batch, page, "html_chrome", name);
        assert_eq!(chrome.metadata.as_ref().unwrap()["chrome"], true);
    }
    assert!(
        topic.metadata.as_ref().unwrap().get("chrome").is_none(),
        "sections are not chrome"
    );
}

#[test]
fn html_links_keep_owners_and_resolve_to_pages_stubs_or_locators() {
    let mut project = topic_project();
    let batch = project.publish(ScanScope::Full);
    let page = "wiki/Topic.html";
    let file = file_of(&batch, page);
    let topic = find(&batch, page, "html_section", "Topic");
    let history = find(&batch, page, "html_section", "History");
    let early = find(&batch, page, "html_section", "Early life");
    let later = find(&batch, page, "html_section", "Later life");
    let legacy = find(&batch, page, "html_section", "Legacy");
    let nav = find(&batch, page, "html_chrome", "nav");
    let header = find(&batch, page, "html_chrome", "header");
    let aside = find(&batch, page, "html_chrome", "aside");
    let footer = find(&batch, page, "html_chrome", "footer");
    let existing = file_of(&batch, "wiki/Existing.html");
    let stub = file_of(&batch, "wiki/Stub.html");

    // Pages declaring a stub mark it on the file element; full pages carry no marker.
    assert_eq!(stub.metadata.as_ref().unwrap()["stub"], true);
    assert!(existing.metadata.as_ref().unwrap().get("stub").is_none());

    // Links before the first heading belong to the file element.
    assert_page(link(&batch, file, "top link"), existing);
    // Same-site links to absent pages produce no edge; chrome owners keep their other links.
    assert_no_link(&batch, nav, "Main page");
    assert_no_link(&batch, header, "Site");
    assert_page(link(&batch, aside, "aside link"), existing);
    assert_no_link(&batch, footer, "Terms");
    // Inline, figure and list links belong to the section that contains them.
    assert_page(link(&batch, topic, "link to existing"), existing);
    assert_external(
        &batch,
        link(&batch, topic, "external"),
        page,
        "https://other.example.net/info?x=1&y=2",
    );
    assert_page(link(&batch, history, "stub page"), stub);
    assert_no_link(&batch, early, "missing");
    assert_page(link(&batch, early, "archive"), existing);
    assert_page(link(&batch, later, "Existing html"), existing);
    // Same-site duplicates from one section collapse: the absolute and relative links name one page.
    assert_eq!(links_from(&batch, later).len(), 1);
    // Javascript, mailto, tel, fragment and script-string hrefs never become links.
    assert!(links_from(&batch, legacy).is_empty());
    // Every links_to names a stored element, so no relationship is left unresolved.
    assert!(
        batch
            .semantic_relationships
            .iter()
            .all(|relationship| relationship.target_element_id.is_some())
    );
    assert!(batch.semantic_relationships.iter().all(|relationship| {
        relationship
            .target_locator
            .as_deref()
            .is_none_or(|locator| !locator.contains("Fake"))
    }));
}

#[test]
fn html_section_identity_follows_visible_words_not_spelling_or_layout() {
    let mut project = Project::new();
    // Escaped and bare ampersands and greater-than signs, and inline tags, spell the same words.
    project.write(
        "spelling/escaped.html",
        "<h2>Notes</h2><p>Tom &amp; Jerry &gt; Spike</p>",
    );
    project.write(
        "spelling/bare.html",
        "<h2>Notes</h2><p>Tom & Jerry > Spike</p>",
    );
    project.write(
        "spelling/inline.html",
        "<h2>Notes</h2><p>Tom <em>&amp;</em> Jerry &gt; <b>Spike</b></p>",
    );
    project.write(
        "spelling/changed.html",
        "<h2>Notes</h2><p>Tom & Jerry > Tyke</p>",
    );
    // Block boundaries separate words; inline boundaries do not.
    project.write(
        "blocks/items.html",
        "<h2>List</h2><ul><li>One</li><li>Two</li></ul>",
    );
    project.write("blocks/run.html", "<h2>List</h2><ul><li>One Two</li></ul>");
    project.write(
        "blocks/joined.html",
        "<h2>List</h2><ul><li>OneTwo</li></ul>",
    );
    project.write("blocks/split.html", "<h2>Word</h2><p>Tom<em>my</em></p>");
    project.write("blocks/whole.html", "<h2>Word</h2><p>Tommy</p>");
    let batch = project.publish(ScanScope::Full);
    let identity = |path: &str, name: &str| section_fingerprints(&batch, path)[name].clone();

    let escaped = identity("spelling/escaped.html", "Notes");
    assert_eq!(identity("spelling/bare.html", "Notes"), escaped);
    assert_eq!(identity("spelling/inline.html", "Notes"), escaped);
    assert_ne!(identity("spelling/changed.html", "Notes"), escaped);
    assert_eq!(
        identity("blocks/run.html", "List"),
        identity("blocks/items.html", "List")
    );
    assert_ne!(
        identity("blocks/joined.html", "List"),
        identity("blocks/items.html", "List")
    );
    assert_eq!(
        identity("blocks/split.html", "Word"),
        identity("blocks/whole.html", "Word")
    );
}

#[test]
fn html_section_identity_ignores_chrome_words() {
    let mut project = topic_project();
    let page = "wiki/Topic.html";
    let baseline = section_fingerprints(&project.publish(ScanScope::Full), page);
    // The footer lies inside the last section's span, but its words are chrome, not content.
    let edited = TOPIC.replace(">Terms</a>", ">Terms of use</a>");
    assert!(edited.contains(">Terms of use</a>"));
    project.write(page, &edited);
    assert_eq!(
        section_fingerprints(&project.publish(ScanScope::Full), page),
        baseline
    );
}

#[test]
fn html_section_fingerprints_follow_words_and_links_not_markup() {
    let mut project = topic_project();
    let page = "wiki/Topic.html";
    let baseline = section_fingerprints(&project.publish(ScanScope::Full), page);
    assert_eq!(baseline.len(), 5);

    // Class names, asset hashes and inline script bodies are markup, not section content.
    let markup_edit = TOPIC
        .replace("class=\"title\"", "class=\"title title-large\"")
        .replace("src=\"x.png\"", "src=\"x.9f3a2b.png\"")
        .replace(
            "document.write(\"<a href='/wiki/Fake'>fake</a>\");",
            "document.write(\"replacement script body\");",
        );
    assert!(markup_edit.contains("title-large"));
    assert!(markup_edit.contains("x.9f3a2b.png"));
    assert!(markup_edit.contains("replacement script body"));
    project.write(page, &markup_edit);
    let after_markup = section_fingerprints(&project.publish(ScanScope::Full), page);
    assert_eq!(after_markup, baseline);

    // One changed word moves only the section that owns it.
    let word_edit = markup_edit.replace("Born somewhere.", "Born nearby.");
    assert!(word_edit.contains("Born nearby."));
    project.write(page, &word_edit);
    let after_word = section_fingerprints(&project.publish(ScanScope::Full), page);
    assert_ne!(after_word["Early life"], baseline["Early life"]);
    for name in ["Topic", "History", "Later life", "Legacy"] {
        assert_eq!(
            after_word[name], baseline[name],
            "section `{name}` must not move"
        );
    }
}

#[test]
fn stub_replaced_by_full_page_keeps_target_identity_for_unchanged_linker() {
    let mut project = topic_project();
    let first = project.publish(ScanScope::Full);
    let page = "wiki/Topic.html";
    let history = find(&first, page, "html_section", "History");
    let stub_before = file_of(&first, "wiki/Stub.html");
    assert_eq!(stub_before.metadata.as_ref().unwrap()["stub"], true);
    assert_page(link(&first, history, "stub page"), stub_before);

    // The full page replaces the stub at the same path; Topic itself does not change.
    project.write("wiki/Stub.html", EXISTING);
    let second = project.publish(ScanScope::Paths(vec!["wiki/Stub.html".into()]));
    let stub_after = file_of(&second, "wiki/Stub.html");
    assert_eq!(
        stub_after.semantic_element_id,
        stub_before.semantic_element_id
    );
    assert!(stub_after.metadata.as_ref().unwrap().get("stub").is_none());
}

#[test]
fn links_follow_target_pages_as_they_appear_and_leave() {
    let mut project = topic_project();
    let first = project.publish(ScanScope::Full);
    let page = "wiki/Topic.html";
    let early = find(&first, page, "html_section", "Early life");
    assert_no_link(&first, early, "missing");

    project.write("wiki/Missing_Page.html", EXISTING);
    let appeared = project.publish(ScanScope::Paths(vec!["wiki/Missing_Page.html".into()]));
    let early_appeared = find(&appeared, page, "html_section", "Early life");
    assert_page(
        link(&appeared, early_appeared, "missing"),
        file_of(&appeared, "wiki/Missing_Page.html"),
    );

    project.remove("wiki/Missing_Page.html");
    let left = project.publish(ScanScope::Paths(vec!["wiki/Missing_Page.html".into()]));
    let early_left = find(&left, page, "html_section", "Early life");
    assert_no_link(&left, early_left, "missing");
}

#[test]
fn html_structure_names_never_resolve_code_references() {
    let mut project = Project::new();
    project.write("app/lib.rs", "fn header() {} fn caller() { header(); }");
    project.write("site/index.html", "<header><p>Site</p></header>");
    let batch = project.publish(ScanScope::Full);
    let caller = find(&batch, "app/lib.rs", "function", "caller");
    let header = find(&batch, "app/lib.rs", "function", "header");
    assert!(batch.semantic_relationships.iter().any(|relationship| {
        relationship.source_element_id == caller.semantic_element_id
            && relationship.target_element_id.as_deref()
                == Some(header.semantic_element_id.as_str())
    }));
}

#[test]
fn html_nested_beyond_the_depth_limit_neither_aborts_nor_drops_shallow_content() {
    let mut project = Project::new();
    project.write("wiki/Existing.html", EXISTING);
    let deep = format!(
        "<h1>Shallow</h1><p><a href=\"/wiki/Existing\">near</a></p>{}buried{}",
        "<div>".repeat(20_000),
        "</div>".repeat(20_000)
    );
    project.write("wiki/Deep.html", &deep);
    let batch = project.publish(ScanScope::Full);
    let page = "wiki/Deep.html";
    let shallow = find(&batch, page, "html_section", "Shallow");
    assert_page(
        link(&batch, shallow, "near"),
        file_of(&batch, "wiki/Existing.html"),
    );
}
