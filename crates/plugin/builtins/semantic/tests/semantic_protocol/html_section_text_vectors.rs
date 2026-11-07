use super::*;

const PAGE: &str = "<html><body><h2>Overview</h2><p>Plain introduction.</p>\
<h2>Details</h2><p>The parser reads each token.</p></body></html>";

#[test]
fn section_body_words_find_their_section_through_text_vectors() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[72; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(Arc::clone(&broker));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");

    // A real project folder: the page is read through project.source, the same way the app reads it.
    let project = tempfile::tempdir().expect("project");
    let root = std::fs::canonicalize(project.path()).expect("canonical project root");
    let root_text = root.to_string_lossy().into_owned();
    std::fs::create_dir_all(root.join("wiki")).expect("wiki folder");
    std::fs::write(root.join("wiki/Page.html"), PAGE).expect("write page");

    // Fingerprints come from the indexer's own section extraction, as they do during indexing.
    let sections = lumvise_project_indexer::html_section_texts(PAGE).expect("sections");
    let section = |name: &str, index: usize| {
        serde_json::json!({
            "semantic_source_id": "source-web", "semantic_element_id": format!("sec:{name}"),
            "path": "wiki/Page.html", "semantic_element_type": "html_section",
            "semantic_element_name": name, "start_line": 1, "end_line": 1,
            "content_fingerprint": sections[index].content_fingerprint,
        })
    };
    let batch = serde_json::json!({
        "provider_instance_id": "web-indexer",
        "project_root": root_text,
        "semantic_sources": [{
            "semantic_source_id": "source-web", "kind": "repository", "name": "web",
            "root_path": root_text, "root_uri": format!("file://{root_text}")
        }],
        "semantic_elements": [
            {"semantic_source_id": "source-web", "semantic_element_id": "file:page",
             "path": "wiki/Page.html", "semantic_element_type": "file",
             "semantic_element_name": "Page.html", "start_line": 1, "end_line": 1},
            section("Overview", 0),
            section("Details", 1),
        ],
        "semantic_relationships": [],
    });
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, broker.observe_fixture(batch))
            .expect("ingest page"),
    );

    *broker.embed_available.lock().expect("embed switch") = true;
    let change = serde_json::json!({
        "project_root": root_text,
        "base_revision": 0,
        "target_revision": 1,
        "changed": [
            {"entity_id": "sec:Overview", "entity_kind": "semantic_element", "disposition": "upserted"},
            {"entity_id": "sec:Details", "entity_kind": "semantic_element", "disposition": "upserted"},
        ]
    });
    success(
        runtime
            .invoke(PLUGIN_ID, ELEMENT_TRIGGER_EXPORT_ID, change)
            .expect("element trigger"),
    );

    // "parser" appears only in the Details body, never in a heading.
    let found = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEARCH_EXPORT_ID,
                serde_json::json!({"project_root": root_text, "query": "parser", "limit": 5}),
            )
            .expect("search"),
    );
    assert_eq!(found["mode"], "vector");
    assert_eq!(
        found["results"][0]["element"]["semantic_element_id"],
        "sec:Details"
    );
}
