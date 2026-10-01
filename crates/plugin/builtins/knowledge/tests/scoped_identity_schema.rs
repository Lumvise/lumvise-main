//! Regression: scoped MCP routing injects `mcp_owner_id`, `plugin_id`, and
//! `session_id` into the arguments before the signed schema validates them.
//! Knowledge's assistant aliases use `additionalProperties: false`, so the
//! schema must declare all three injected fields or scoped tool calls fail
//! with "Additional properties are not allowed ('mcp_owner_id' was unexpected)".

use lumvise_plugin_knowledge::{
    ASSISTANT_FIND_ELEMENTS_EXPORT_ID, ASSISTANT_GET_ELEMENT_EXPORT_ID, ASSISTANT_GET_EXPORT_ID,
    ASSISTANT_LIST_EXPORT_ID, ASSISTANT_SEARCH_EXPORT_ID, package_manifest_source,
};
use lumvise_plugin_package::ExportSurface;

const SCOPED_IDENTITY_FIELDS: &[&str] = &["mcp_owner_id", "plugin_id", "session_id"];

#[test]
fn assistant_alias_schemas_declare_scoped_route_identity_fields() {
    let manifest = package_manifest_source("test-target", &"a".repeat(64));

    for export in manifest
        .exports
        .iter()
        .filter(|export| matches!(export.surface, ExportSurface::ScopedMcpTool { .. }))
    {
        assert_eq!(
            export.input_schema["additionalProperties"],
            serde_json::Value::Bool(false),
            "scoped export `{}` must keep a closed input schema",
            export.id
        );
        let properties = export.input_schema["properties"]
            .as_object()
            .unwrap_or_else(|| {
                panic!(
                    "scoped export `{}` must expose object properties",
                    export.id
                )
            });
        for field in SCOPED_IDENTITY_FIELDS {
            assert_eq!(
                properties[*field]["type"], "string",
                "scoped export `{}` must declare injected identity field `{field}` as string",
                export.id
            );
        }
    }
}

#[test]
fn knowledge_find_elements_alias_remains_closed_with_scoped_identity() {
    let manifest = package_manifest_source("test-target", &"a".repeat(64));
    let export = manifest
        .exports
        .iter()
        .find(|export| export.id == ASSISTANT_FIND_ELEMENTS_EXPORT_ID)
        .expect("assistant find_elements alias exists");

    assert_eq!(
        export.input_schema["additionalProperties"],
        serde_json::Value::Bool(false)
    );
    assert_eq!(export.input_schema["properties"]["query"]["type"], "string");
    assert_eq!(
        export.input_schema["properties"]["mcp_owner_id"]["type"],
        "string"
    );
    assert_eq!(
        export.input_schema["properties"]["plugin_id"]["type"],
        "string"
    );
    assert_eq!(
        export.input_schema["properties"]["session_id"]["type"],
        "string"
    );
}

#[test]
fn all_assistant_aliases_are_scoped() {
    let manifest = package_manifest_source("test-target", &"a".repeat(64));
    for id in [
        ASSISTANT_FIND_ELEMENTS_EXPORT_ID,
        ASSISTANT_GET_ELEMENT_EXPORT_ID,
        ASSISTANT_GET_EXPORT_ID,
        ASSISTANT_LIST_EXPORT_ID,
        ASSISTANT_SEARCH_EXPORT_ID,
    ] {
        assert!(manifest.exports.iter().any(|export| {
            export.id == id && matches!(export.surface, ExportSurface::ScopedMcpTool { .. })
        }));
    }
}

#[test]
fn packaged_knowledge_path_schemas_match_runtime_contract() {
    let template: serde_json::Value =
        serde_json::from_str(include_str!("../lumvise-plugin-manifest.json")).unwrap();
    let runtime = lumvise_plugin_knowledge::package_manifest_source("test-target", &"a".repeat(64));
    for export in runtime.exports {
        let packaged = template["exports"]
            .as_array()
            .unwrap()
            .iter()
            .find(|candidate| candidate["id"] == export.id)
            .unwrap();
        assert_eq!(
            packaged["input_schema"], export.input_schema,
            "input schema for {}",
            export.id
        );
        assert_eq!(
            packaged["output_schema"], export.output_schema,
            "output schema for {}",
            export.id
        );
    }
}
