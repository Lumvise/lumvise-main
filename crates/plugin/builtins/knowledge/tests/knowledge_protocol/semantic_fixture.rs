//! Signed Semantic fixture. Ingestion uses the real owner's schema and implementation;
//! the other exports provide the named query answers used by the protocol tests.
use super::*;

const TARGET: &str = "knowledge-semantic-fixture-host";
const EXPORTS: [&str; 5] = [
    "get_semantic_tree",
    "search_semantic_elements",
    "semantic_graph",
    "semantic_context",
    "ingest_index_batch",
];

pub(super) fn signed_semantic_fixture(
    workspace: &Path,
    signing_key: &SigningKey,
) -> InstalledPlugin {
    let executable = std::fs::read(env!("CARGO_BIN_EXE_knowledge-test-semantic-plugin"))
        .expect("read Semantic fixture binary");
    let digest = hex::encode(Sha256::digest(&executable));
    let manifest = fixture_manifest(&digest);
    let executable_path = manifest.targets[TARGET].clone();
    let archive = workspace.join("semantic-fixture.lvp");
    build_package(
        &archive,
        BuildPackageRequest {
            manifest,
            files: BTreeMap::from([(executable_path, executable)]),
            signing_key,
        },
    )
    .expect("build Semantic fixture package");
    verify_fixture(workspace, &archive, signing_key)
}

fn fixture_manifest(digest: &str) -> PluginManifest {
    let mut manifest = lumvise_plugin_semantic::package_manifest_source(TARGET, digest);
    manifest.publisher = PublisherIdentity {
        publisher_id: "lumvise.test".into(),
        key_id: "lumvise.test.semantic.1".into(),
    };
    manifest.plugin_version = "0.1.0".into();
    manifest.exports = fixture_exports(manifest.exports);
    manifest.host_capabilities = vec![lumvise_plugin_package::HostCapabilityRequirement {
        id: "fixture.semantic.ingest".into(),
        version: "^1.0".into(),
    }];
    manifest
}

fn fixture_exports(exports: Vec<ExportDescriptor>) -> Vec<ExportDescriptor> {
    exports
        .into_iter()
        .filter(|export| EXPORTS.contains(&export.id.as_str()))
        .map(|mut export| {
            if export.id != lumvise_plugin_semantic::INGEST_EXPORT_ID {
                export.input_schema = serde_json::json!({"type": "object"});
                export.output_schema = serde_json::json!({"type": "object"});
            }
            export
        })
        .collect()
}

fn verify_fixture(workspace: &Path, archive: &Path, signing_key: &SigningKey) -> InstalledPlugin {
    let verified = verify_package(
        archive,
        &signing_key.verifying_key(),
        &HostCompatibility::new(PACKAGE_PROTOCOL_VERSION, TARGET),
    )
    .expect("verify Semantic fixture package");
    install_verified_package(&verified, &workspace.join("semantic-installed"))
        .expect("install Semantic fixture package")
}
