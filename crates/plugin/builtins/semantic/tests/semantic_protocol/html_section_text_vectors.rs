use super::*;
use lumvise_plugin_runtime::PluginSystem;
use std::path::Path;

use super::support;

const PAGE: &str = "<html><body><h2>Overview</h2><p>Plain introduction.</p>\
<h2>Details</h2><p>The parser reads each token.</p></body></html>";

const WIKI_A: &str = "<html><body><h2>One</h2><p>alpha text.</p><h2>Two</h2><p>beta text.</p>\
<h2>Three</h2><p>gamma text.</p></body></html>";
const WIKI_B: &str = "<html><body><h2>Solo</h2><p>delta text.</p></body></html>";

fn start_plugin(workspace: &Path, broker: &Arc<MemoryCapabilityBroker>) -> PluginSystem {
    let (_, installed) = signed_install(workspace, &SigningKey::from_bytes(&[73; 32]));
    let runtime = system(Arc::clone(broker));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    runtime
}

/// Writes `pages` into a real project folder and returns its canonical root.
fn write_project(dir: &Path, pages: &[(&str, &str)]) -> String {
    let root = std::fs::canonicalize(dir).expect("canonical project root");
    for (path, html) in pages {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().expect("page folder")).expect("page folder");
        std::fs::write(file, html).expect("write page");
    }
    root.to_string_lossy().into_owned()
}

/// Ingests every page and its `html_section`s. Fingerprints come from the indexer's own
/// extraction, as they do during indexing.
fn ingest_pages(
    runtime: &PluginSystem,
    broker: &MemoryCapabilityBroker,
    root_text: &str,
    pages: &[(&str, &str)],
) {
    let mut elements = Vec::new();
    for (path, html) in pages {
        elements.push(serde_json::json!({
            "semantic_source_id": "source-web", "semantic_element_id": format!("file:{path}"),
            "path": path, "semantic_element_type": "file",
            "semantic_element_name": path, "start_line": 1, "end_line": 1,
        }));
        let sections = lumvise_project_indexer::html_section_texts(html).expect("sections");
        for (index, section) in sections.iter().enumerate() {
            elements.push(serde_json::json!({
                "semantic_source_id": "source-web",
                "semantic_element_id": format!("sec:{path}#{index}"),
                "path": path, "semantic_element_type": "html_section",
                "semantic_element_name": format!("section {index}"),
                "start_line": 1, "end_line": 1,
                "content_fingerprint": section.content_fingerprint,
            }));
        }
    }
    let batch = serde_json::json!({
        "provider_instance_id": "web-indexer",
        "project_root": root_text,
        "semantic_sources": [{
            "semantic_source_id": "source-web", "kind": "repository", "name": "web",
            "root_path": root_text, "root_uri": format!("file://{root_text}")
        }],
        "semantic_elements": elements,
        "semantic_relationships": [],
    });
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, broker.observe_fixture(batch))
            .expect("ingest pages"),
    );
}

fn rebuild(runtime: &PluginSystem, root_text: &str) -> WireOutcome {
    support::run_rebuild(
        runtime,
        serde_json::json!({"action":"start","project_root":root_text,"operation_id":"full-rebuild","mode":"full"}),
    )
}

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

#[test]
fn rejected_generation_preserves_completed_sections_without_claiming_ready() {
    let workspace = tempfile::tempdir().expect("workspace");
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = start_plugin(workspace.path(), &broker);
    let project = tempfile::tempdir().expect("project");
    let pages = [("wiki/A.html", WIKI_A), ("wiki/B.html", WIKI_B)];
    let root_text = write_project(project.path(), &pages);
    ingest_pages(&runtime, &broker, &root_text, &pages);
    *broker.embed_available.lock().expect("embed switch") = true;
    broker.reject_text_vectors_for("sec:wiki/A.html#1");

    let rebuilt = success(rebuild(&runtime, &root_text));

    assert_eq!(rebuilt["status"], "failed");
    assert_eq!(rebuilt["index_state"], "failed");
    assert_eq!(rebuilt["progress"]["sections_completed"], 1);
    assert_eq!(rebuilt["failures"][0]["element_id"], "sec:wiki/A.html#1");
    assert_eq!(rebuilt["failures"][0]["code"], "rebuild_step_failed");
}

#[test]
fn unreadable_source_keeps_completed_progress_and_reports_incomplete_coverage() {
    let workspace = tempfile::tempdir().expect("workspace");
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = start_plugin(workspace.path(), &broker);
    let project = tempfile::tempdir().expect("project");
    // Missing.html is indexed but never written, so project.source cannot read it.
    let root_text = write_project(project.path(), &[("wiki/A.html", WIKI_A)]);
    ingest_pages(
        &runtime,
        &broker,
        &root_text,
        &[("wiki/A.html", WIKI_A), ("wiki/Missing.html", WIKI_B)],
    );
    *broker.embed_available.lock().expect("embed switch") = true;

    let rebuilt = success(rebuild(&runtime, &root_text));

    assert_eq!(rebuilt["status"], "failed");
    assert_eq!(rebuilt["index_state"], "failed");
    assert_eq!(rebuilt["progress"]["sections_completed"], 3);
    assert_eq!(rebuilt["failures"][0]["path"], "wiki/Missing.html");
    assert_eq!(
        rebuilt["failures"][0]["element_id"],
        "sec:wiki/Missing.html#0"
    );
}

#[test]
fn rejected_name_storage_reports_safe_failed_run_without_raw_host_error() {
    let workspace = tempfile::tempdir().expect("workspace");
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = start_plugin(workspace.path(), &broker);
    let project = tempfile::tempdir().expect("project");
    let root_text = write_project(project.path(), &[("wiki/A.html", WIKI_A)]);
    ingest_pages(&runtime, &broker, &root_text, &[("wiki/A.html", WIKI_A)]);
    *broker.embed_available.lock().expect("embed switch") = true;
    broker.fail_store_operation("store_element_name_vectors");

    let result = success(rebuild(&runtime, &root_text));
    assert_eq!(result["status"], "failed");
    assert_eq!(result["failures"][0]["code"], "rebuild_step_failed");
    assert!(!result.to_string().contains("injected_store_failure"));
}

const PARSER_PAGE: &str =
    "<html><body><h2>Parsing</h2><p>parser reads each token.</p></body></html>";
const RENDER_PAGE: &str =
    "<html><body><h2>Drawing</h2><p>render paints each frame.</p></body></html>";
const PARSER_SECTION: &str = "sec:wiki/Parse.html#0";
const RENDER_SECTION: &str = "sec:wiki/Draw.html#0";

/// Ingests both pages with the embedder on. Name vectors are not built yet.
fn ingest_parser_and_render(
    broker: &Arc<MemoryCapabilityBroker>,
    runtime: &PluginSystem,
    root: &str,
) {
    ingest_pages(
        runtime,
        broker,
        root,
        &[
            ("wiki/Parse.html", PARSER_PAGE),
            ("wiki/Draw.html", RENDER_PAGE),
        ],
    );
    *broker.embed_available.lock().expect("embed switch") = true;
}

fn rebuild_with(runtime: &PluginSystem, root_text: &str, extra: serde_json::Value) -> WireOutcome {
    let mut input = serde_json::json!({"action":"start","project_root":root_text,"operation_id":format!("case-{extra}"),"mode":"full"});
    for (key, value) in extra.as_object().expect("extra input object") {
        input[key] = value.clone();
    }
    support::run_rebuild(runtime, input)
}

/// The whole search response; the score is 0 when the element is not in the results.
fn search(runtime: &PluginSystem, root_text: &str, query: &str) -> serde_json::Value {
    success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEARCH_EXPORT_ID,
                serde_json::json!({"project_root": root_text, "query": query, "limit": 100}),
            )
            .expect("search"),
    )
}

fn score_of(response: &serde_json::Value, semantic_element_id: &str) -> f64 {
    response["results"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|result| result["element"]["semantic_element_id"] == semantic_element_id)
        .map_or(0.0, |result| result["score"].as_f64().expect("score"))
}

#[test]
fn names_mode_sets_ready_and_stores_no_text_vectors() {
    let workspace = tempfile::tempdir().expect("workspace");
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = start_plugin(workspace.path(), &broker);
    let project = tempfile::tempdir().expect("project");
    let root_text = write_project(
        project.path(),
        &[
            ("wiki/Parse.html", PARSER_PAGE),
            ("wiki/Draw.html", RENDER_PAGE),
        ],
    );
    ingest_parser_and_render(&broker, &runtime, &root_text);

    let rebuilt = success(rebuild_with(
        &runtime,
        &root_text,
        serde_json::json!({"mode": "names"}),
    ));

    assert_eq!(rebuilt["index_state"], "ready");
    assert_eq!(rebuilt["status"], "complete");
    assert_eq!(rebuilt["progress"]["chunks_staged"], 0);
    assert!(rebuilt["continuation"].is_null());
    // The body word matches only a text vector; names-only leaves that section unmatched.
    assert_eq!(
        score_of(&search(&runtime, &root_text, "parser"), PARSER_SECTION),
        0.0
    );
}

#[test]
fn text_mode_stores_only_the_given_paths_and_keeps_the_index_state() {
    let workspace = tempfile::tempdir().expect("workspace");
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = start_plugin(workspace.path(), &broker);
    let project = tempfile::tempdir().expect("project");
    let root_text = write_project(
        project.path(),
        &[
            ("wiki/Parse.html", PARSER_PAGE),
            ("wiki/Draw.html", RENDER_PAGE),
        ],
    );
    ingest_parser_and_render(&broker, &runtime, &root_text);
    success(rebuild_with(
        &runtime,
        &root_text,
        serde_json::json!({"mode": "names"}),
    ));

    let first = success(rebuild_with(
        &runtime,
        &root_text,
        serde_json::json!({"mode": "text", "paths": ["wiki/Parse.html"]}),
    ));

    assert_eq!(first["progress"]["sections_completed"], 1);
    assert_eq!(first["status"], "complete");
    assert!(first["continuation"].is_null());
    let after_first = search(&runtime, &root_text, "parser");
    assert!(score_of(&after_first, PARSER_SECTION) > 0.0);
    assert_eq!(after_first["index_state"], "ready");
    // The page that was not requested still has no text vector.
    assert_eq!(
        score_of(&search(&runtime, &root_text, "render"), RENDER_SECTION),
        0.0
    );

    // A second call with the remaining path completes the work and still keeps the state.
    let second = success(rebuild_with(
        &runtime,
        &root_text,
        serde_json::json!({"mode": "text", "paths": ["wiki/Draw.html"]}),
    ));
    assert_eq!(second["progress"]["sections_completed"], 1);
    assert_eq!(second["status"], "complete");
    let after_second = search(&runtime, &root_text, "render");
    assert!(score_of(&after_second, RENDER_SECTION) > 0.0);
    assert_eq!(after_second["index_state"], "ready");
}

#[test]
fn paths_are_rejected_outside_text_mode() {
    let workspace = tempfile::tempdir().expect("workspace");
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = start_plugin(workspace.path(), &broker);
    let project = tempfile::tempdir().expect("project");
    let root_text = write_project(project.path(), &[("wiki/Parse.html", PARSER_PAGE)]);
    ingest_pages(
        &runtime,
        &broker,
        &root_text,
        &[("wiki/Parse.html", PARSER_PAGE)],
    );

    let error = failure(rebuild_with(
        &runtime,
        &root_text,
        serde_json::json!({"mode": "names", "paths": ["wiki/Parse.html"]}),
    ));

    assert_eq!(error.code, "invalid_semantic_input");
}

#[test]
fn long_section_is_embedded_and_stored_as_one_vector_per_chunk() {
    let workspace = tempfile::tempdir().expect("workspace");
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = start_plugin(workspace.path(), &broker);
    let project = tempfile::tempdir().expect("project");
    let long_page = format!(
        "<html><body><h2>Long</h2><p>{}</p></body></html>",
        "word ".repeat(1500)
    );
    let pages = [("wiki/Long.html", long_page.as_str())];
    let root_text = write_project(project.path(), &pages);
    ingest_pages(&runtime, &broker, &root_text, &pages);
    *broker.embed_available.lock().expect("embed switch") = true;

    let rebuilt = success(rebuild(&runtime, &root_text));

    // The fixture planner assigns at most 1,022 source characters per 1,024-token range.
    assert_eq!(rebuilt["status"], "complete");
    assert_eq!(rebuilt["progress"]["chunks_staged"], 8);
    assert_eq!(rebuilt["progress"]["sections_completed"], 1);
    assert!(broker.embed_tasks().iter().all(|task| task == "document"));
}

#[test]
fn rejected_chunk_stops_run_without_hiding_uncovered_section() {
    let workspace = tempfile::tempdir().expect("workspace");
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = start_plugin(workspace.path(), &broker);
    let project = tempfile::tempdir().expect("project");
    let mixed_page = format!(
        "<html><body><h2>Good</h2><p>parser text.</p>\
<h2>Bad</h2><p>{} text.</p></body></html>",
        support::EMBED_REJECTED_MARKER
    );
    let pages = [("wiki/Mixed.html", mixed_page.as_str())];
    let root_text = write_project(project.path(), &pages);
    ingest_pages(&runtime, &broker, &root_text, &pages);
    *broker.embed_available.lock().expect("embed switch") = true;

    let rebuilt = success(rebuild(&runtime, &root_text));

    assert_eq!(rebuilt["status"], "failed");
    assert_eq!(rebuilt["progress"]["sections_completed"], 1);
    assert_eq!(
        rebuilt["failures"][0]["element_id"],
        "sec:wiki/Mixed.html#1"
    );
    assert!(
        !rebuilt["failures"]
            .to_string()
            .contains(support::EMBED_REJECTED_MARKER)
    );
}
