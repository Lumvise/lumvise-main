//! Agent guidance is delivered through the public signed package manifest.
use lumvise_plugin_knowledge::package_manifest_source;
use lumvise_plugin_package::{ExportDescriptor, ExportSurface, PluginManifest};

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
fn assistant_aliases_reuse_public_guidance_without_changing_scope() {
    let manifest = packaged_manifest();
    for (public, alias) in [
        ("create_knowledge", "knowledge.create"),
        ("update_knowledge", "knowledge.update"),
        ("get_knowledge", "knowledge.get"),
        ("search_knowledge", "knowledge.search"),
        ("list_knowledge_for_element", "knowledge.list_for_element"),
        ("list_knowledge_dependents", "knowledge.list_dependents"),
        ("find_semantic_elements", "knowledge.find_elements"),
        ("get_semantic_element", "knowledge.get_element"),
    ] {
        assert_eq!(
            export(&manifest, public).description,
            export(&manifest, alias).description
        );
        assert!(
            matches!(&export(&manifest, alias).surface, ExportSurface::ScopedMcpTool { scope } if scope == "assistant_session")
        );
    }
}

#[test]
fn task_writes_explain_readable_records_and_real_update_semantics() {
    let manifest = packaged_manifest();
    let create = &export(&manifest, "create_knowledge").description;
    for concept in [
        "search_knowledge",
        "get_knowledge",
        "project_root",
        "narrowest",
        "plain titles/content",
        "task_assignment",
        "status",
        "owner",
        "acceptance criteria",
        "blockers",
        "evidence",
    ] {
        assert!(
            create.to_lowercase().contains(concept),
            "missing task concept {concept}"
        );
    }
    let update = &export(&manifest, "update_knowledge").description;
    for concept in [
        "search_knowledge/get_knowledge",
        "preserve human text",
        "replace prior values",
        "merge deliberately",
        "verified",
        "commands/results",
        "no atomic lease/claim guarantee",
    ] {
        assert!(
            update.contains(concept),
            "missing update constraint {concept}"
        );
    }
}

#[test]
fn checkpoint_reads_require_scope_and_current_evidence_before_resume() {
    let manifest = packaged_manifest();
    let read = export(&manifest, "get_knowledge");
    assert!(
        read.input_schema["properties"]
            .get("project_root")
            .is_none()
    );
    for concept in [
        "no project_root field",
        "returned artifact's project_root",
        "checkpoints/handoffs",
        "current source/tests",
        "before resuming",
    ] {
        assert!(
            read.description.contains(concept),
            "missing restore constraint {concept}"
        );
    }
    assert!(
        export(&manifest, "search_knowledge")
            .description
            .contains("not full content")
    );
    assert!(
        export(&manifest, "get_semantic_element")
            .description
            .contains("view omits project_root")
    );
    assert!(
        export(&manifest, "find_semantic_elements")
            .description
            .contains("requires that plugin to be enabled/available")
    );
}

#[test]
fn transfer_guidance_requires_review_and_describes_nonatomic_copy_results() {
    let manifest = packaged_manifest();
    let preview = &export(&manifest, "preview_knowledge_transfer").description;
    for concept in [
        "Read-only",
        "destination project_root",
        "source_project_root",
        "transfer_ids",
        "not project identity",
        "does not apply transfers automatically",
    ] {
        assert!(
            preview.contains(concept),
            "missing preview constraint {concept}"
        );
    }
    let apply = &export(&manifest, "apply_knowledge_transfer").description;
    for concept in [
        "reviewed transfer_ids",
        "fresh preview",
        "Revalidates",
        "source artifacts remain",
        "interrupted copies",
        "copied_artifact_ids",
        "not an atomic batch or lease/claim",
    ] {
        assert!(
            apply.contains(concept),
            "missing apply constraint {concept}"
        );
    }
}

#[test]
fn runtime_and_http_guidance_do_not_invent_project_filters_or_completion() {
    let manifest = packaged_manifest();
    assert!(
        export(&manifest, "http.knowledge.events")
            .description
            .contains("not project-filtered")
    );
    assert!(
        export(&manifest, "ensure_c4_nucleus")
            .description
            .contains("a target_id or target_path")
    );
    assert!(
        export(&manifest, "http.knowledge.c4_nucleus")
            .description
            .contains("in the JSON body")
    );
    assert!(
        export(&manifest, "rebuild_knowledge_vectors")
            .description
            .contains("unavailable")
    );
    assert!(
        export(&manifest, "poll_functional_artifact_generation")
            .description
            .contains("not an implementation-task scheduler")
    );
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
