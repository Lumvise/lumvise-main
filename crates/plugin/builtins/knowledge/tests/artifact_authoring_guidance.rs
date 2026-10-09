use lumvise_plugin_knowledge::package_manifest_source;
use lumvise_plugin_package::PluginManifest;

const AUTHORING_EXPORT_IDS: [&str; 6] = [
    "create_knowledge",
    "knowledge.create",
    "update_knowledge",
    "knowledge.update",
    "http.knowledge.artifacts.create",
    "http.knowledge.artifacts.update",
];

const AUTHORING_REQUIREMENTS: [&str; 7] = [
    "human-readable Markdown",
    "plain language",
    "short paragraphs",
    "blank lines",
    "agent conversation",
    "metadata",
    "terse agent shorthand",
];
const PACKAGED_MANIFEST: &str = include_str!("../lumvise-plugin-manifest.json");

#[test]
fn artifact_writing_tools_deliver_human_readable_markdown_guidance() {
    let generated = package_manifest_source("test-target", &"a".repeat(64));
    for id in AUTHORING_EXPORT_IDS {
        let export = generated
            .exports
            .iter()
            .find(|export| export.id == id)
            .unwrap();
        for requirement in AUTHORING_REQUIREMENTS {
            assert!(
                export.description.contains(requirement),
                "{id} lacks authoring requirement {requirement:?}"
            );
        }
    }
}

#[test]
fn packaged_artifact_writing_guidance_matches_the_public_manifest() {
    let generated = package_manifest_source("test-target", &"a".repeat(64));
    let packaged: PluginManifest = serde_json::from_str(PACKAGED_MANIFEST).unwrap();
    for id in AUTHORING_EXPORT_IDS {
        let expected = generated
            .exports
            .iter()
            .find(|export| export.id == id)
            .unwrap();
        assert!(
            packaged
                .exports
                .iter()
                .any(|shipped| shipped.id == id && shipped.description == expected.description),
            "{id} ships stale authoring guidance"
        );
    }
}
