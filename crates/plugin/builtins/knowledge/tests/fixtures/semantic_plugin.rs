use lumvise_plugin_sdk::{PluginApplication, PluginContext, PluginError};
use serde_json::{Value, json};

struct SemanticFixture;

impl PluginApplication for SemanticFixture {
    fn plugin_id(&self) -> &str {
        "builtin.semantic"
    }

    fn dispatch(
        &self,
        capability_id: &str,
        input: Value,
        _context: &mut PluginContext<'_>,
    ) -> Result<Value, PluginError> {
        match capability_id {
            "get_semantic_tree" => Ok(tree()),
            "search_semantic_elements" => Ok(search()),
            "semantic_graph" => Ok(graph()),
            "semantic_context" => Ok(context(input)),
            other => Err(PluginError::unknown_capability(other)),
        }
    }
}

fn main() {
    if let Err(error) = lumvise_plugin_sdk::run_stdio(&SemanticFixture) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn element_view() -> Value {
    json!({
        "semantic_element_id": "element-a", "element_kind": "function",
        "name": "element_a", "path": "src/a.rs", "parent_element_id": null,
        "start_line": 1, "end_line": 8
    })
}

fn tree() -> Value {
    json!({"project_root": "/project", "semantic_element_id": "element-a",
        "total_nodes": 1, "roots": [{"element": element_view(), "children": []}]})
}

fn search() -> Value {
    json!({"query": "Element", "project_root": "/project", "mode": "lexical_fallback",
        "results": [{"element": element_view(), "score": 1.0}]})
}

fn graph() -> Value {
    json!({
        "providerId": "/project", "providerName": "project", "dashboardUrl": null,
        "projectRoot": "/project", "targetPath": null, "granularity": "property",
        "source": "app-owned-index", "generatedAt": "2026-07-11T00:00:00Z",
        "summary": "fixture", "nodes": [{
            "id": "element-a", "label": "element_a", "kind": "function",
            "parentId": null, "parentLabel": null, "depth": 0, "isContainer": false,
            "path": "src/a.rs", "lineStart": 1, "lineEnd": 8, "codeSize": 8,
            "connectionStrength": 0, "semanticArtifactCount": 1, "commentCount": 0,
            "dataSemanticArtifactCount": 1, "summary": "Function A",
            "artifacts": [], "stableRef": "element-a"
        }], "edges": []
    })
}

fn context(input: Value) -> Value {
    let record_kind = input["record_kind"].as_str().unwrap_or_default();
    let elements = json!([{
        "project_root": "/project", "semantic_element_id": "file:a",
        "semantic_source_id": "source-main", "path": "src/a.rs",
        "element_kind": "file", "name": "a.rs", "parent_element_id": null,
        "content_fingerprint": "fp1:0000000000000001:file-a", "start_line": 1, "end_line": 20,
        "lifecycle": "active", "metadata": {"indexer_metadata": {}}
    }, {
        "project_root": "/project", "semantic_element_id": "element-a",
        "semantic_source_id": "source-main", "path": "src/a.rs",
        "element_kind": "function", "name": "element_a", "parent_element_id": "file:a",
        "content_fingerprint": "fp1:0000000000000002:a", "start_line": 1, "end_line": 8,
        "lifecycle": "active", "metadata": {"indexer_metadata": {
            "signature": "pub fn element_a(input: &str) -> Result<()>"
        }}
    }]);
    let relationships = json!([{
        "project_root": "/project", "source_element_id": "file:a",
        "target_element_id": "element-a", "relationship_kind": "contains",
        "label": "contains", "lifecycle": "active", "metadata": {}
    }]);
    let artifacts = json!([{
        "project_root": "/project", "artifact_id": "source-element-a",
        "semantic_element_id": "element-a", "artifact_kind": "source",
        "title": "element_a source", "content_ref": null,
        "content": "pub fn element_a(input: &str) -> Result<()> { serde_json::json!({}); Ok(()) }",
        "searchable_text": "element a", "content_size_bytes": 80,
        "metadata": {"language": "rust"}
    }]);
    json!({
        "record_kind": record_kind,
        "elements": if record_kind == "elements" { elements } else { json!([]) },
        "relationships": if record_kind == "relationships" { relationships } else { json!([]) },
        "artifacts": if record_kind == "artifacts" { artifacts } else { json!([]) },
        "next_after_key": null
    })
}
