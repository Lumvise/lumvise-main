use serde_json::{Value, json};

use crate::{
    KnowledgeArtifact, KnowledgeKind, c4_report, c4_text, functional, obsidian_reports,
    project_structure,
    semantic_context::{SemanticContext, SemanticElement, SemanticRelationship},
};

#[test]
fn functional_summary_matches_canonical_fixture() {
    let source = "pub fn render(input: Result<Vec<Value>, Error>) -> Result<String, Error> {\n    serde_json::to_string(&input)?\n}\n";
    let element = semantic_element("fn:render", "src/render.rs", "function", "render");
    let actual = functional::golden_summary(&compiled_element(element), source);
    assert_eq!(
        actual,
        fixture(include_str!(
            "../tests/fixtures/golden/functional_summary.json"
        ))
    );
}

#[test]
fn c4_text_and_paths_match_canonical_fixture() {
    let element = compiled_element(semantic_element(
        "fn:route",
        "src/gateway.rs",
        "function",
        "route/request",
    ));
    let text = "Returns `Result<Vec<Value>>` & status, then routes | output.";
    let actual = json!({
        "text": c4_text::c4_text(text),
        "target_path": c4_text::c4_target_path(&element),
        "report_path": c4_text::c4_report_path(&element),
        "project_note_link": c4_text::project_note_link(&element, "Route <request>")
    });
    assert_eq!(
        actual,
        fixture(include_str!("../tests/fixtures/golden/c4_text_paths.json"))
    );
}

#[test]
fn scoped_c4_report_matches_canonical_fixture() {
    let target = semantic_element("folder:src", "src", "folder", "src");
    let elements = scoped_elements(&target);
    let relationships = scoped_relationships();
    let elements: Vec<SemanticElement> =
        serde_json::from_value(elements).expect("compiled scoped elements");
    let relationships: Vec<SemanticRelationship> =
        serde_json::from_value(relationships).expect("compiled scoped relationships");
    let report = c4_report::report(
        "/repo",
        &compiled_element(target),
        &elements,
        &relationships,
        &[],
        "schema:c4-report-v6|fixture",
    );
    let projection = obsidian_reports::project("/repo", &report);
    let actual = json!({
        "content": report.content,
        "missing_functional_element_ids": report.metadata["nucleus"]["missing_functional_element_ids"],
        "projection": {
            "contentMd5": projection["contentMd5"],
            "markdown": projection["markdown"],
            "pathHint": projection["pathHint"],
            "title": projection["title"]
        }
    });
    assert_eq!(
        actual,
        fixture(include_str!(
            "../tests/fixtures/golden/scoped_c4_report.json"
        ))
    );
}

#[test]
fn nucleus_projection_title_uses_artifact_kind_label() {
    let mut report = artifact(
        "nucleus-derived-summary",
        "folder:src",
        "Stored report title",
        "Derived report body",
        json!({"obsidian": {"path": "src/C4 Architecture.md"}}),
    );
    report.tags = vec!["nucleus".into(), "cultivation-report".into()];

    let projection = obsidian_reports::project("/repo", &report);

    assert_eq!(report.title, "Stored report title");
    assert_eq!(
        projection["title"],
        Value::String("C4 Architecture (derived-summary)".into())
    );
}

#[test]
fn project_knowledge_matches_canonical_fixture() {
    let (context, artifacts) = project_fixture();
    let actual = project_structure::project("/repo", context, artifacts)
        .expect("compiled project projection");
    assert_eq!(
        actual,
        fixture(include_str!(
            "../tests/fixtures/golden/project_knowledge.json"
        ))
    );
}

#[test]
fn project_file_nests_typed_artifacts_under_semantic_elements() {
    let (context, artifacts) = project_fixture();
    let projection = project_structure::project("/repo", context, artifacts)
        .expect("compiled project projection");
    let markdown = projection["elements"][0]["markdown"]
        .as_str()
        .expect("project file markdown");

    assert!(
        markdown.contains(
            "### run\n- Kind: `function`\n- Element ID: `fn:run`\n- Project context: [[src/lib.rs/run|run]]\n\n#### Artifacts\n\n##### Job (`responsibility`)\n\nRoutes requests to storage."
        ),
        "unexpected project file markdown: {markdown}"
    );
}

#[test]
fn project_knowledge_edges_match_canonical_fixture() {
    let (context, artifacts) = edge_fixture();
    let actual =
        project_structure::project("/repo", context, artifacts).expect("compiled edge projection");
    assert_eq!(
        actual,
        fixture(include_str!(
            "../tests/fixtures/golden/project_knowledge_edges.json"
        ))
    );
    assert!(
        actual["elements"]
            .as_array()
            .unwrap()
            .iter()
            .all(|element| {
                element["syncToken"]
                    .as_str()
                    .is_some_and(|marker| marker.len() <= 64)
            })
    );
}

fn semantic_element(id: &str, path: &str, kind: &str, name: &str) -> Value {
    json!({"project_root": "/repo", "semantic_element_id": id,
        "semantic_source_id": "source-main", "path": path, "element_kind": kind,
        "name": name, "parent_element_id": null, "content_fingerprint": "fp1:0000000000000001:fixture",
        "start_line": 1, "end_line": 3, "lifecycle": "active", "match_evidence": null,
        "metadata": {"indexer_metadata": {}}})
}

fn child(mut value: Value, parent_id: &str) -> Value {
    value["parent_element_id"] = json!(parent_id);
    value
}

fn compiled_element(value: Value) -> SemanticElement {
    serde_json::from_value(value).expect("compiled semantic element fixture")
}

fn scoped_elements(target: &Value) -> Value {
    json!([
        target,
        child(
            semantic_element("file:gateway", "src/gateway.rs", "file", "Gateway <HTTP>"),
            "folder:src"
        ),
        child(
            semantic_element("file:storage", "src/storage.rs", "file", "Storage & Index"),
            "folder:src"
        ),
        semantic_element(
            "service:embedding",
            "services/embedding.rs",
            "service",
            "Embedding | API"
        )
    ])
}

fn scoped_relationships() -> Value {
    json!([
        {"project_root": "/repo", "source_element_id": "file:gateway",
            "target_element_id": "file:storage", "relationship_kind": "calls",
            "label": "calls", "lifecycle": "active", "metadata": {}},
        {"project_root": "/repo", "source_element_id": "file:gateway",
            "target_element_id": "service:embedding", "relationship_kind": "uses",
            "label": "routes_to", "lifecycle": "active", "metadata": {}}
    ])
}

fn project_fixture() -> (SemanticContext, Vec<KnowledgeArtifact>) {
    let elements = json!([
        semantic_element("file:src/lib.rs", "src/lib.rs", "file", "lib.rs"),
        child(
            semantic_element("fn:run", "src/lib.rs", "function", "run"),
            "file:src/lib.rs"
        ),
        semantic_element("file:src/store.rs", "src/store.rs", "file", "store.rs")
    ]);
    let relationships = json!([{"project_root": "/repo", "source_element_id": "fn:run",
        "target_element_id": "file:src/store.rs", "relationship_kind": "calls",
        "label": "calls", "lifecycle": "active", "metadata": {"confidence": 1}}]);
    let metadata = json!({"job": "Routes requests to storage.", "receives": ["request"],
        "outcome": "Stored response.", "effects": ["Writes state."],
        "source_interface": "fn run(request: Request) -> Result<Response>"});
    (
        semantic_context(elements, relationships),
        vec![artifact(
            "functional-run",
            "fn:run",
            "Functional meaning: run",
            "Functional body",
            metadata,
        )],
    )
}

fn edge_fixture() -> (SemanticContext, Vec<KnowledgeArtifact>) {
    let elements = json!([
        semantic_element("folder:docs", "docs", "folder", "docs"),
        child(
            semantic_element("directory:guides", "docs/guides", "directory", "guides"),
            "folder:docs"
        ),
        child(
            semantic_element("file:context", "CONTEXT.md", "file", "CONTEXT.md"),
            "folder:docs"
        ),
        child(
            semantic_element("markdown:first", "CONTEXT.md", "markdown", "Repeated"),
            "file:context"
        ),
        child(
            semantic_element("markdown:second", "CONTEXT.md", "markdown", "Repeated"),
            "file:context"
        )
    ]);
    let resolved = json!({"job": "Maintains guide hierarchy.", "receives": ["documents"],
        "outcome": "Guides grouped.", "effects": ["Writes index."],
        "source_interface": "fn group(documents: &[Document])"});
    let fallback = json!({"function": "Explains repeated Markdown sections.",
        "transformation": "Converts context into guidance.",
        "receives": ["No explicit parameters are indexed; infer inputs from local context."],
        "effects": ["No obvious external side effect is visible from the indexed source span."]});
    (
        semantic_context(elements, json!([])),
        vec![
            artifact(
                "functional-guides",
                "directory:guides",
                "Guide job",
                "Resolved guide body",
                resolved,
            ),
            artifact(
                "functional-markdown",
                "markdown:first",
                "Markdown job",
                "## Job\nExplains repeated Markdown sections.",
                fallback,
            ),
        ],
    )
}

fn semantic_context(elements: Value, relationships: Value) -> SemanticContext {
    SemanticContext {
        elements: serde_json::from_value(elements).expect("compiled elements"),
        relationships: serde_json::from_value::<Vec<SemanticRelationship>>(relationships)
            .expect("compiled relationships"),
        artifacts: Vec::new(),
    }
}

fn artifact(
    id: &str,
    owner: &str,
    title: &str,
    content: &str,
    metadata: Value,
) -> KnowledgeArtifact {
    KnowledgeArtifact {
        artifact_id: id.into(),
        semantic_element_id: owner.into(),
        knowledge_type: KnowledgeKind::DerivedSummary,
        title: title.into(),
        content: content.into(),
        tags: vec!["cultivation-functional".into()],
        dependencies: Vec::new(),
        metadata,
        path: None,
        project_root: Some("/repo".into()),
    }
}

fn fixture(source: &str) -> Value {
    serde_json::from_str(source).expect("canonical Knowledge JSON fixture")
}
