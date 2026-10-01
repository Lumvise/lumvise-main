use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
};

use ed25519_dalek::SigningKey;
use lumvise_plugin_package::{PluginManifest, PluginReleaseIndexV2, ReleaseComposition};

use crate::{
    BuiltinBinaryPaths, BuiltinReleaseDescriptor, BuiltinReleaseError, BuiltinReleaseMatrix,
    Result, discover_release_descriptors, publish_builtin_release_composition,
};

const SUPPORTED_RELEASE_TARGETS: [&str; 1] = ["aarch64-apple-darwin"];

/// Cargo profile used to compile built-in plugin executables.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuiltinCompilationProfile {
    /// Fast developer build with debug symbols.
    Debug,
    /// Optimized, locked production build.
    Release,
}

impl BuiltinCompilationProfile {
    fn output_directory(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }
}

/// Compiles every descriptor-discovered built-in plugin for each requested target.
///
/// Cargo output remains outside package staging; the returned matrix points
/// only at final release executables.
///
/// # Example
/// ```no_run
/// # use std::path::Path;
/// let matrix = lumvise_builtin_plugin_release::build_builtin_binaries(
///     Path::new("."),
///     &["aarch64-apple-darwin".into()],
///     Path::new("target/builtin-release"),
/// )?;
/// assert_eq!(matrix.targets.len(), 1);
/// # Ok::<(), lumvise_builtin_plugin_release::BuiltinReleaseError>(())
/// ```
pub fn build_builtin_binaries(
    workspace_root: &Path,
    targets: &[String],
    target_dir: &Path,
) -> Result<BuiltinReleaseMatrix> {
    build_builtin_binaries_with_profile(
        workspace_root,
        targets,
        target_dir,
        BuiltinCompilationProfile::Release,
    )
}

/// Compiles all built-in plugins with an explicit Cargo profile.
pub fn build_builtin_binaries_with_profile(
    workspace_root: &Path,
    targets: &[String],
    target_dir: &Path,
    profile: BuiltinCompilationProfile,
) -> Result<BuiltinReleaseMatrix> {
    build_builtin_binaries_composition_with_profile(
        &[workspace_root.to_path_buf()],
        targets,
        target_dir,
        profile,
        ReleaseComposition::Full,
    )
}

/// Compiles only the plugins selected by one explicit product composition.
pub fn build_builtin_binaries_composition_with_profile(
    workspace_roots: &[PathBuf],
    targets: &[String],
    target_dir: &Path,
    profile: BuiltinCompilationProfile,
    composition: ReleaseComposition,
) -> Result<BuiltinReleaseMatrix> {
    if targets.is_empty() {
        return Err(invalid_targets(targets));
    }
    let target_dir = std::path::absolute(target_dir).map_err(|source| BuiltinReleaseError::Io {
        path: target_dir.to_path_buf(),
        source,
    })?;
    let mut descriptors = discover_release_descriptors(workspace_roots)?
        .into_iter()
        .map(|descriptor| (descriptor.plugin_id.clone(), descriptor))
        .collect::<BTreeMap<_, _>>();
    if composition == ReleaseComposition::Minimal {
        descriptors.retain(|plugin_id, _| {
            matches!(plugin_id.as_str(), "builtin.semantic" | "builtin.knowledge")
        });
        if descriptors.len() != 2 {
            return Err(BuiltinReleaseError::InvalidInput {
                value: descriptors.keys().cloned().collect::<Vec<_>>().join(","),
                expected: "minimal descriptors builtin.knowledge,builtin.semantic".into(),
            });
        }
    }
    let manifests = read_manifest_templates(&descriptors)?;
    let mut binaries = BTreeMap::new();
    for target in targets {
        validate_release_target(target)?;
        for (root, selected) in descriptors_by_workspace(&descriptors) {
            compile_target(&root, &target_dir, target, profile, &selected)?;
        }
        binaries.insert(
            target.clone(),
            target_binaries(&target_dir, target, profile, &descriptors),
        );
    }
    let matrix = BuiltinReleaseMatrix {
        descriptors,
        manifests,
        targets: binaries,
    };
    matrix.validate()?;
    Ok(matrix)
}

fn validate_release_target(target: &str) -> Result<()> {
    if SUPPORTED_RELEASE_TARGETS.contains(&target) {
        return Ok(());
    }
    Err(BuiltinReleaseError::InvalidInput {
        value: target.into(),
        expected: format!(
            "production target with proven sandbox support: {}",
            SUPPORTED_RELEASE_TARGETS.join(", ")
        ),
    })
}

/// Compiles and publishes one explicit product composition.
pub fn build_and_publish_builtin_release_composition_with_profile(
    workspace_root: &Path,
    targets: &[String],
    target_dir: &Path,
    signing_key: &SigningKey,
    output_dir: &Path,
    profile: BuiltinCompilationProfile,
    composition: ReleaseComposition,
) -> Result<PluginReleaseIndexV2> {
    let matrix = build_builtin_binaries_composition_with_profile(
        &[workspace_root.to_path_buf()],
        targets,
        target_dir,
        profile,
        composition,
    )?;
    publish_builtin_release_composition(&matrix, signing_key, output_dir, composition)
}

fn descriptors_by_workspace(
    descriptors: &BTreeMap<String, BuiltinReleaseDescriptor>,
) -> BTreeMap<PathBuf, BTreeMap<String, BuiltinReleaseDescriptor>> {
    let mut workspaces = BTreeMap::<PathBuf, BTreeMap<String, BuiltinReleaseDescriptor>>::new();
    for (plugin_id, descriptor) in descriptors {
        workspaces
            .entry(descriptor.workspace_root.clone())
            .or_default()
            .insert(plugin_id.clone(), descriptor.clone());
    }
    workspaces
}

fn compile_target(
    workspace_root: &Path,
    target_dir: &Path,
    target: &str,
    profile: BuiltinCompilationProfile,
    descriptors: &BTreeMap<String, BuiltinReleaseDescriptor>,
) -> Result<()> {
    let status = cargo_build_command(workspace_root, target_dir, target, profile, descriptors)
        .status()
        .map_err(|error| compilation_error(target, error.to_string()))?;
    if status.success() {
        return Ok(());
    }
    Err(compilation_error(target, status.to_string()))
}

fn cargo_build_command(
    workspace_root: &Path,
    target_dir: &Path,
    target: &str,
    profile: BuiltinCompilationProfile,
    descriptors: &BTreeMap<String, BuiltinReleaseDescriptor>,
) -> Command {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .current_dir(workspace_root)
        .arg("build")
        .arg("--manifest-path")
        .arg(workspace_root.join("Cargo.toml"));
    if profile == BuiltinCompilationProfile::Release {
        command.arg("--release");
    }
    command.args(["--locked", "--target", target, "--target-dir"]);
    command.arg(target_dir);
    for descriptor in descriptors.values() {
        command.args([
            "--package",
            descriptor.package.as_str(),
            "--bin",
            descriptor.binary.as_str(),
        ]);
    }
    command
}

fn compilation_error(target: &str, status: String) -> BuiltinReleaseError {
    BuiltinReleaseError::Compilation {
        target: target.into(),
        status,
    }
}

fn target_binaries(
    target_dir: &Path,
    target: &str,
    profile: BuiltinCompilationProfile,
    descriptors: &BTreeMap<String, BuiltinReleaseDescriptor>,
) -> BuiltinBinaryPaths {
    let output = target_dir.join(target).join(profile.output_directory());
    descriptors
        .iter()
        .map(|(plugin_id, descriptor)| {
            (
                plugin_id.clone(),
                executable(&output, &descriptor.binary, target),
            )
        })
        .collect()
}

fn read_manifest_templates(
    descriptors: &BTreeMap<String, BuiltinReleaseDescriptor>,
) -> Result<BTreeMap<String, PluginManifest>> {
    descriptors
        .iter()
        .map(|(plugin_id, descriptor)| {
            let path = descriptor
                .workspace_root
                .join(&descriptor.crate_path)
                .join(&descriptor.manifest_template);
            let bytes = std::fs::read(&path).map_err(|source| BuiltinReleaseError::Io {
                path: path.clone(),
                source,
            })?;
            let manifest = serde_json::from_slice(&bytes)?;
            Ok((plugin_id.clone(), manifest))
        })
        .collect()
}
fn executable(release: &Path, name: &str, target: &str) -> PathBuf {
    let suffix = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    release.join(format!("{name}{suffix}"))
}

fn invalid_targets(targets: &[String]) -> BuiltinReleaseError {
    BuiltinReleaseError::InvalidInput {
        value: format!("targets: {targets:?}"),
        expected: "at least one supported Rust host target".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unproven_release_targets_are_rejected() {
        for target in ["x86_64-unknown-linux-gnu", "x86_64-apple-darwin", "custom"] {
            let error = validate_release_target(target).expect_err("unsupported target");
            assert!(error.to_string().contains("aarch64-apple-darwin"));
        }
    }

    #[test]
    fn future_windows_binary_path_uses_executable_suffix() {
        let path = executable(Path::new("release"), "plugin", "x86_64-pc-windows-msvc");

        assert_eq!(path, Path::new("release/plugin.exe"));
    }

    #[test]
    fn debug_profile_omits_release_flag_and_uses_debug_output() {
        let descriptors = test_descriptors();
        let command = cargo_build_command(
            Path::new("workspace"),
            Path::new("target/plugins"),
            "aarch64-apple-darwin",
            BuiltinCompilationProfile::Debug,
            &descriptors,
        );
        let arguments = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(!arguments.iter().any(|value| value == "--release"));
        assert!(
            target_binaries(
                Path::new("target/plugins"),
                "aarch64-apple-darwin",
                BuiltinCompilationProfile::Debug,
                &descriptors,
            )["builtin.example"]
                .ends_with("debug/lumvise-plugin-example")
        );
    }

    fn test_descriptors() -> BTreeMap<String, BuiltinReleaseDescriptor> {
        let descriptor = BuiltinReleaseDescriptor {
            package: "lumvise-plugin-example".into(),
            binary: "lumvise-plugin-example".into(),
            plugin_id: "builtin.example".into(),
            plugin_version: "1.0.0".into(),
            publisher_id: "lumvise.builtins".into(),
            key_id: "lumvise-builtin-release".into(),
            protocol_min: 1,
            protocol_max: 1,
            payload_pattern: "plugins/builtin.example/{target}/example".into(),
            manifest_template: "lumvise-plugin-manifest.json".into(),
            payload_assets_dir: None,
            payload_assets_path: None,
            crate_path: PathBuf::from("crates/plugin/builtins/example"),
            workspace_root: PathBuf::from("workspace"),
        };
        BTreeMap::from([(descriptor.plugin_id.clone(), descriptor)])
    }
}
