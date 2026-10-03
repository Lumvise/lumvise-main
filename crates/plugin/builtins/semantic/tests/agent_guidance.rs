//! Guidance is part of the public signed manifest, not a separate agent guide.
use lumvise_plugin_package::{ExportDescriptor, PluginManifest};
use lumvise_plugin_semantic::package_manifest_source;

fn packaged_manifest() -> PluginManifest {
    package_manifest_source("test-target", &"a".repeat(64))
}

fn export<'a>(manifest: &'a PluginManifest, id: &str) -> &'a ExportDescriptor {
    manifest
        .exports
        .iter()
        .find(|export| export.id == id)
        .unwrap()
}

fn without_descriptions(mut manifest: PluginManifest) -> serde_json::Value {
    for export in &mut manifest.exports {
        export.description.clear();
    }
    serde_json::to_value(manifest.exports).unwrap()
}

#[test]
fn every_signed_export_has_concise_capability_guidance() {
    for export in packaged_manifest().exports {
        assert!(
            export.description.len() >= 40,
            "missing guidance for {}",
            export.id
        );
        assert!(
            export.description.len() <= 1200,
            "oversized guidance for {}",
            export.id
        );
        assert_ne!(export.description, export.name);
    }
}

#[test]
fn guidance_preserves_existing_export_and_host_contracts() {
    let template: PluginManifest =
        serde_json::from_str(include_str!("../lumvise-plugin-manifest.json")).unwrap();
    let runtime = packaged_manifest();
    assert_eq!(
        serde_json::to_value(&runtime.protocol).unwrap(),
        serde_json::to_value(&template.protocol).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&runtime.host_capabilities).unwrap(),
        serde_json::to_value(&template.host_capabilities).unwrap()
    );
    assert_eq!(
        without_descriptions(runtime),
        without_descriptions(template)
    );
}

#[test]
fn existing_analysis_summaries_remain_in_signed_descriptions() {
    let runtime = packaged_manifest();
    for (id, summary) in [
        ("search_graph", "Ranked symbol search"),
        ("get_code_snippet", "Read current source"),
        ("search_code", "Ranked text or regex search"),
        ("get_graph_schema", "Summarize observed element kinds"),
        ("trace_path", "Trace calls with direction"),
        ("get_git_impact", "Read git changes"),
        ("get_graph_metrics", "Report fan-in/out hotspots"),
        ("check_index_coverage", "Report per-file parser coverage"),
        ("ingest_runtime_trace", "Replace one named runtime trace"),
    ] {
        let description = &export(&runtime, id).description;
        assert!(description.starts_with(summary), "lost summary for {id}");
        assert!(description.contains("project_root"));
    }
}

#[test]
fn discovery_guidance_connects_owner_impact_source_and_validation() {
    let manifest = packaged_manifest();
    let tree = &export(&manifest, "get_semantic_tree").description;
    for concept in [
        "project_root",
        "semantic_element_id",
        "get_dependency_tree",
        "trace_path",
        "get_git_impact",
        "check_index_coverage",
        "focused tests",
    ] {
        assert!(
            tree.contains(concept),
            "missing discovery concept {concept}"
        );
    }
    assert!(
        export(&manifest, "graph_providers")
            .description
            .contains("not project identity")
    );
    assert!(
        export(&manifest, "get_dependency_tree")
            .description
            .contains("no project_root field")
    );
}

#[test]
fn freshness_guidance_distinguishes_index_source_and_vector_rebuilds() {
    let manifest = packaged_manifest();
    assert!(
        export(&manifest, "get_code_snippet")
            .description
            .contains("index_matches_source")
    );
    assert!(
        export(&manifest, "get_code_snippet")
            .description
            .contains("false or unknown")
    );
    assert!(
        export(&manifest, "search_code")
            .description
            .contains("current worktree")
    );
    assert!(
        export(&manifest, "check_index_coverage")
            .description
            .contains("not dead-code proof")
    );
    assert!(
        export(&manifest, "rebuild_search_index")
            .description
            .contains("does not parse changed source")
    );
    let ingest = &export(&manifest, "ingest_index_batch").description;
    for concept in [
        "project_root",
        "indexer",
        "compiler",
        "never fabricate",
        "replace_paths",
        "snapshot pages",
    ] {
        assert!(
            ingest.contains(concept),
            "missing ingestion constraint {concept}"
        );
    }
}

#[test]
fn evidence_tools_do_not_claim_complete_or_static_runtime_resolution() {
    let manifest = packaged_manifest();
    assert!(
        export(&manifest, "get_git_impact")
            .description
            .contains("unindexed_paths")
    );
    assert!(
        export(&manifest, "trace_path")
            .description
            .contains("not proof of no callers")
    );
    assert!(
        export(&manifest, "get_graph_metrics")
            .description
            .contains("hypotheses")
    );
    assert!(
        export(&manifest, "ingest_runtime_trace")
            .description
            .contains("only measured calls")
    );
    assert!(
        export(&manifest, "ingest_runtime_trace")
            .description
            .contains("does not repair or replace")
    );
    let snapshot = export(&manifest, "create_semantic_snapshot");
    assert!(
        snapshot.input_schema["properties"]
            .get("destination_path")
            .is_none()
    );
    assert!(!snapshot.description.contains("destination_path"));
}

#[test]
fn release_template_descriptions_match_public_manifest() {
    let template: PluginManifest =
        serde_json::from_str(include_str!("../lumvise-plugin-manifest.json")).unwrap();
    let runtime = packaged_manifest();
    assert_eq!(template.plugin_version, runtime.plugin_version);
    for signed in template.exports {
        assert_eq!(
            signed.description,
            export(&runtime, &signed.id).description,
            "stale signed guidance for {}",
            signed.id
        );
    }
}
