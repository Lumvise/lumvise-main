use lumvise_plugin_package::{ExecutionMode, ExportDescriptor, ExportSurface};
use serde_json::{Value, json};

const TOOLS: &[(&str, &str, &[&str])] = &[
    (
        "search_graph",
        "Ranked symbol search with regex name/path filters and stable offset pagination.",
        &[
            "query",
            "name_pattern",
            "path_pattern",
            "element_kind",
            "case_sensitive",
            "offset",
            "limit",
        ],
    ),
    (
        "get_code_snippet",
        "Read current source for an indexed element; reports index/source fingerprint agreement.",
        &["semantic_element_id", "context_lines"],
    ),
    (
        "search_code",
        "Ranked text or regex search of indexed files with innermost symbol context.",
        &[
            "query",
            "regex",
            "case_sensitive",
            "name_pattern",
            "path_pattern",
            "element_kind",
            "offset",
            "limit",
        ],
    ),
    (
        "get_graph_schema",
        "Summarize observed element kinds, edge labels and public fields.",
        &[],
    ),
    (
        "trace_path",
        "Trace calls with direction, depth and edge-label filters; unknown receivers are excluded.",
        &[
            "semantic_element_id",
            "direction",
            "max_depth",
            "relationship_labels",
            "path_pattern",
            "element_kind",
        ],
    ),
    (
        "get_git_impact",
        "Read git changes against a base commit and find transitive callers at file granularity.",
        &["base", "max_depth", "relationship_labels", "path_pattern"],
    ),
    (
        "get_graph_metrics",
        "Report fan-in/out hotspots, cyclic components and heuristic dead-code candidates.",
        &[
            "relationship_labels",
            "name_pattern",
            "path_pattern",
            "element_kind",
            "offset",
            "limit",
        ],
    ),
    (
        "check_index_coverage",
        "Report per-file parser coverage, syntax recovery and resolution outcomes; old indexes require reindexing.",
        &["path_pattern", "offset", "limit"],
    ),
    (
        "ingest_runtime_trace",
        "Replace one named runtime trace of observed calls between indexed IDs; preserve static extraction.",
        &["trace_id", "edges"],
    ),
];

pub(crate) fn supports(id: &str) -> bool {
    TOOLS.iter().any(|(name, _, _)| *name == id)
}

pub(crate) fn exports() -> Vec<ExportDescriptor> {
    TOOLS
        .iter()
        .map(|(id, description, fields)| ExportDescriptor {
            id: (*id).into(),
            name: id.replace('_', " "),
            description: crate::manifest::analysis_description(id, description),
            surface: ExportSurface::McpTool,
            input_schema: input_schema(id, fields),
            output_schema: output_schema(id),
            admission: None,
            execution: ExecutionMode::Foreground,
        })
        .collect()
}

fn output_schema(id: &str) -> Value {
    match id {
        "search_code" => json!({
            "type":"object",
            "properties":{
                "matches": {"type":"array","items":{"type":"object","properties":{
                    "path":{"type":"string"},"line":{"type":"integer"},"text":{"type":"string"},
                    "element":{"type":["object","null"]},"score":{"type":"integer"}
                },"required":["path","line","text","element","score"]}},
                "failures":{"type":"array","items":{"type":"object","properties":{
                    "path":{"type":"string"},"error":{"type":"string"}
                },"required":["path","error"]}},
                "failure_count":{"type":"integer","minimum":0},
                "total":{"type":"integer"},"offset":{"type":"integer"},
                "next_offset":{"type":["integer","null"]},"source_revision":{"type":"string"},
                "commit_version":{"type":"integer"},"published_at":{"type":"string"}
            },
            "required":["matches","failures","failure_count","total","offset","next_offset","source_revision","commit_version","published_at"],
            "additionalProperties":false
        }),
        "get_graph_metrics" => json!({
            "type":"object",
            "properties":{
                "element_count":{"type":"integer"},"edge_count":{"type":"integer"},
                "hotspots":metric_page_schema(json!({"type":"object","properties":{
                    "semantic_element_id":{"type":"string"},"fan_in":{"type":"integer"},"fan_out":{"type":"integer"}
                },"required":["semantic_element_id","fan_in","fan_out"]})),
                "cycles":metric_page_schema(json!({"type":"array","items":{"type":"string"}})),
                "dead_code_candidates":metric_page_schema(json!({"type":"object","properties":{
                    "semantic_element_id":{"type":"string"},"name":{"type":"string"},"path":{"type":"string"},
                    "element_kind":{"type":"string"},"start_line":{"type":["integer","null"]}
                },"required":["semantic_element_id","name","path","element_kind","start_line"]})),
                "caveat":{"type":"string"},"commit_version":{"type":"integer"},"published_at":{"type":"string"}
            },
            "required":["element_count","edge_count","hotspots","cycles","dead_code_candidates","caveat","commit_version","published_at"],
            "additionalProperties":false
        }),
        _ => json!({"type":"object"}),
    }
}

fn metric_page_schema(row: Value) -> Value {
    json!({"type":"object","properties":{
        "matches":{"type":"array","items":row},"total":{"type":"integer"},"offset":{"type":"integer"},
        "next_offset":{"type":["integer","null"]}
    },"required":["matches","total","offset","next_offset"],"additionalProperties":false})
}

fn input_schema(id: &str, fields: &[&str]) -> Value {
    let mut properties = serde_json::Map::from_iter([
        (
            "project_root".into(),
            json!({"type":"string","minLength":1}),
        ),
        (
            "expected_commit_version".into(),
            json!({"type":"integer","description":"Reject stale pagination when the static index changes."}),
        ),
    ]);
    for field in fields {
        properties.insert((*field).into(), field_schema(field));
    }
    let mut required = vec!["project_root"];
    match id {
        "get_code_snippet" | "trace_path" => required.push("semantic_element_id"),
        "search_code" => required.push("query"),
        "ingest_runtime_trace" => required.extend(["trace_id", "edges"]),
        _ => {}
    }
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

fn field_schema(field: &str) -> Value {
    match field {
        "regex" | "case_sensitive" => json!({"type":"boolean"}),
        "offset" | "context_lines" | "max_depth" => json!({"type":"integer","minimum":0}),
        "limit" => json!({"type":"integer","minimum":1,"default":50}),
        "direction" => {
            json!({"type":"string","enum":["inbound","outbound","both"],"default":"outbound"})
        }
        "relationship_labels" => {
            json!({"type":"array","items":{"type":"string"},"default":["calls"],"description":"Empty array includes all non-containment relationships."})
        }
        "edges" => {
            json!({"type":"array","items":{"type":"object","required":["source_element_id","target_element_id"],"additionalProperties":false,
            "properties":{"source_element_id":{"type":"string"},"target_element_id":{"type":"string"},"count":{"type":"integer","minimum":1,"default":1}}}})
        }
        _ => json!({"type":"string"}),
    }
}
