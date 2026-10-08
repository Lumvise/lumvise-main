//! Owns the CLI's development signing identity; source release metadata stays unchanged.
//! Call `select` before compilation, then `apply` before publishing the release matrix.

use ed25519_dalek::VerifyingKey;
use lumvise_builtin_plugin_release::{BuiltinCompilationProfile, BuiltinReleaseMatrix};

const DEVELOPMENT_PUBLIC_KEY: &str =
    "6b734a8eff246fe734b38d4046c148eee5f04fe87b3a0a423955a77956de066b";
const BUILTIN_PUBLISHER: &str = "lumvise.builtin";
const DEVELOPMENT_KEY_ID: &str = "lumvise.release.1";
const PRODUCTION_KEY_ID: &str = "lumvise.production.1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReleaseSigningIdentity {
    Configured,
    Development,
}

impl ReleaseSigningIdentity {
    pub(super) fn select(
        profile: BuiltinCompilationProfile,
        verification_key: &VerifyingKey,
    ) -> Result<Self, String> {
        let public_key = hex::encode(verification_key.to_bytes());
        if public_key != DEVELOPMENT_PUBLIC_KEY {
            return Ok(Self::Configured);
        }
        if profile == BuiltinCompilationProfile::Debug {
            return Ok(Self::Development);
        }
        Err(format!(
            "release signing key {public_key}; expected a protected key whose private seed is not the public development fixture"
        ))
    }

    pub(super) fn apply(self, matrix: &mut BuiltinReleaseMatrix) -> Result<(), String> {
        if self == Self::Configured {
            return Ok(());
        }
        require_builtin_identities(matrix)?;
        // Debug launch scripts trust the fixture authority, never the production key ID.
        for descriptor in matrix.descriptors.values_mut() {
            descriptor.key_id = DEVELOPMENT_KEY_ID.into();
        }
        for manifest in matrix.manifests.values_mut() {
            manifest.publisher.key_id = DEVELOPMENT_KEY_ID.into();
        }
        Ok(())
    }
}

fn require_builtin_identities(matrix: &BuiltinReleaseMatrix) -> Result<(), String> {
    for (plugin, descriptor) in &matrix.descriptors {
        let matches = matrix.manifests.get(plugin).is_some_and(|manifest| {
            descriptor.publisher_id == BUILTIN_PUBLISHER
                && [PRODUCTION_KEY_ID, DEVELOPMENT_KEY_ID].contains(&descriptor.key_id.as_str())
                && manifest.publisher.publisher_id == descriptor.publisher_id
                && manifest.publisher.key_id == descriptor.key_id
        });
        if !matches {
            return Err(format!(
                "debug plugin {plugin} publisher/key {:?}/{:?}; expected matching {BUILTIN_PUBLISHER} production or development identities",
                descriptor.publisher_id, descriptor.key_id
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use lumvise_builtin_plugin_release::{
        BuiltinBinaryPaths, BuiltinReleaseDescriptor, discover_descriptors,
        publish_builtin_release_composition,
    };
    use lumvise_plugin_package::{
        HostCompatibility, PluginManifest, ReleaseComposition, verify_package,
    };
    use std::{collections::BTreeMap, fs, path::Path};

    struct ReleaseIdentityFixture {
        root: tempfile::TempDir,
        matrix: BuiltinReleaseMatrix,
    }

    impl ReleaseIdentityFixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(4)
                .unwrap();
            let descriptors = discover_descriptors(workspace)
                .unwrap()
                .into_iter()
                .map(|descriptor| (descriptor.plugin_id.clone(), descriptor))
                .collect::<BTreeMap<_, _>>();
            let manifests = Self::read_templates(workspace, &descriptors);
            let binaries = Self::write_binaries(root.path(), &descriptors);
            let matrix = BuiltinReleaseMatrix {
                descriptors,
                manifests,
                targets: BTreeMap::from([("aarch64-apple-darwin".into(), binaries)]),
            };
            Self { root, matrix }
        }

        fn read_templates(
            workspace: &Path,
            descriptors: &BTreeMap<String, BuiltinReleaseDescriptor>,
        ) -> BTreeMap<String, PluginManifest> {
            descriptors
                .iter()
                .map(|(plugin, descriptor)| {
                    let path = workspace
                        .join(&descriptor.crate_path)
                        .join(&descriptor.manifest_template);
                    (
                        plugin.clone(),
                        serde_json::from_slice(&fs::read(path).unwrap()).unwrap(),
                    )
                })
                .collect()
        }

        fn write_binaries(
            root: &Path,
            descriptors: &BTreeMap<String, BuiltinReleaseDescriptor>,
        ) -> BuiltinBinaryPaths {
            descriptors
                .keys()
                .map(|plugin| {
                    let path = root.join(plugin);
                    fs::write(&path, plugin).unwrap();
                    (plugin.clone(), path)
                })
                .collect()
        }
    }

    #[test]
    fn public_fixture_is_debug_only_and_configured_keys_keep_their_identity() {
        let bytes = hex::decode(DEVELOPMENT_PUBLIC_KEY).unwrap();
        let fixture = VerifyingKey::from_bytes(&bytes.try_into().unwrap()).unwrap();
        assert_eq!(
            ReleaseSigningIdentity::select(BuiltinCompilationProfile::Debug, &fixture).unwrap(),
            ReleaseSigningIdentity::Development
        );
        let error = ReleaseSigningIdentity::select(BuiltinCompilationProfile::Release, &fixture)
            .unwrap_err();
        assert!(error.contains(DEVELOPMENT_PUBLIC_KEY));
        assert!(error.contains("expected a protected key"));
        let protected = SigningKey::from_bytes(&[97; 32]).verifying_key();
        for profile in [
            BuiltinCompilationProfile::Release,
            BuiltinCompilationProfile::Debug,
        ] {
            assert_eq!(
                ReleaseSigningIdentity::select(profile, &protected).unwrap(),
                ReleaseSigningIdentity::Configured
            );
        }
    }

    #[test]
    fn debug_publication_uses_development_authority_without_changing_source_metadata() {
        let mut fixture = ReleaseIdentityFixture::new();
        let original = fixture.matrix.clone();
        ReleaseSigningIdentity::Development
            .apply(&mut fixture.matrix)
            .unwrap();
        let key = SigningKey::from_bytes(&[97; 32]);
        let output = fixture.root.path().join("release");
        let index = publish_builtin_release_composition(
            &fixture.matrix,
            &key,
            &output,
            ReleaseComposition::Minimal,
        )
        .unwrap();
        assert_eq!(index.artifacts.len(), 2);
        let trust: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("publisher-trust.json")).unwrap())
                .unwrap();
        assert_eq!(trust["keys"][0]["key_id"], DEVELOPMENT_KEY_ID);
        for artifact in index.artifacts {
            assert_eq!(artifact.key_id, DEVELOPMENT_KEY_ID);
            let host = HostCompatibility::new(6, "aarch64-apple-darwin");
            let verified = verify_package(
                &output.join(&artifact.archive_path),
                &key.verifying_key(),
                &host,
            )
            .unwrap();
            assert_eq!(verified.manifest().publisher.key_id, DEVELOPMENT_KEY_ID);
            assert_eq!(
                serde_json::to_value(&verified.manifest().exports).unwrap(),
                serde_json::to_value(&original.manifests[&artifact.plugin_id].exports).unwrap()
            );
        }
        assert!(
            original
                .descriptors
                .values()
                .all(|descriptor| descriptor.key_id == PRODUCTION_KEY_ID)
        );
    }

    #[test]
    fn configured_publication_is_unchanged_and_invalid_debug_identity_is_not_rewritten() {
        let mut fixture = ReleaseIdentityFixture::new();
        let original = serde_json::to_value(&fixture.matrix).unwrap();
        ReleaseSigningIdentity::Configured
            .apply(&mut fixture.matrix)
            .unwrap();
        assert_eq!(serde_json::to_value(&fixture.matrix).unwrap(), original);
        fixture
            .matrix
            .descriptors
            .get_mut("builtin.knowledge")
            .unwrap()
            .publisher_id = "administrator".into();
        let invalid = serde_json::to_value(&fixture.matrix).unwrap();
        assert!(
            ReleaseSigningIdentity::Development
                .apply(&mut fixture.matrix)
                .unwrap_err()
                .contains("administrator")
        );
        assert_eq!(serde_json::to_value(&fixture.matrix).unwrap(), invalid);
    }
}
