use std::{env, fs, path::Path, process::Command};

use lumvise_builtin_plugin_release::{
    BuiltinReleaseMatrix, build_builtin_binaries_composition_with_profile,
    publish_builtin_release_composition,
};
use lumvise_plugin_package::read_protected_signing_key;

mod native_diagnostics;
mod options;
mod release_signing_identity;
use native_diagnostics::NativeSymbols;
use options::ReleaseOptions;
use release_signing_identity::ReleaseSigningIdentity;

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("built-in plugin release failed: {error}");
        std::process::exit(2);
    }
}

fn run(arguments: Vec<String>) -> Result<(), String> {
    if arguments
        .first()
        .is_some_and(|value| value == "--verify-release")
    {
        return verify_prepared_release(&arguments[1..]);
    }
    let current = env::current_dir().map_err(|error| error.to_string())?;
    let options = ReleaseOptions::parse(arguments, &current)?;
    let key = read_protected_signing_key(&options.key).map_err(|error| error.to_string())?;
    let identity = ReleaseSigningIdentity::select(options.profile, &key.verifying_key())?;
    let mut matrix = build_builtin_binaries_composition_with_profile(
        &options.workspaces,
        &options.targets,
        &options.target_dir,
        options.profile,
        options.composition,
    )
    .map_err(|error| error.to_string())?;
    identity.apply(&mut matrix)?;
    let staging = tempfile::tempdir().map_err(|error| error.to_string())?;
    let matrix = stage_payloads(&matrix, staging.path())?;
    // Apple signatures must be inside the payload before its Ed25519 digest is computed.
    let diagnostics = env::var_os("LUMVISE_RELEASE_DIAGNOSTICS_DIR").map(std::path::PathBuf::from);
    prepare_macos_plugins(
        &matrix,
        env::var("APPLE_SIGNING_IDENTITY").ok().as_deref(),
        diagnostics.as_deref(),
    )?;
    let index =
        publish_builtin_release_composition(&matrix, &key, &options.output, options.composition)
            .map_err(|error| error.to_string())?;
    // Both editions must carry the very same signed plugin bytes. Signing a
    // second time can change timestamps without changing the plugin version.
    if let Some(output) = options.community_output {
        publish_builtin_release_composition(
            &matrix,
            &key,
            &output,
            lumvise_plugin_package::ReleaseComposition::Minimal,
        )
        .map_err(|error| error.to_string())?;
    }
    println!(
        "published {} built-in plugin packages",
        index.artifacts.len()
    );
    Ok(())
}

fn verify_prepared_release(arguments: &[String]) -> Result<(), String> {
    let [key, directory, target] = arguments else {
        return Err(format!(
            "verification arguments {arguments:?}; expected --verify-release <protected-signing-key> <release-dir> <target>"
        ));
    };
    let key = read_protected_signing_key(Path::new(key)).map_err(|error| error.to_string())?;
    let directory = Path::new(directory);
    let index =
        lumvise_plugin_package::PluginReleaseIndexV2::read(directory.join("builtins-release.json"))
            .map_err(|error| error.to_string())?;
    index
        .verify_artifacts(directory)
        .map_err(|error| error.to_string())?;
    let host = lumvise_plugin_package::HostCompatibility::new(
        u32::from(lumvise_plugin_protocol::CURRENT_PROTOCOL_VERSION.major),
        target,
    );
    for artifact in &index.artifacts {
        lumvise_plugin_package::verify_package(
            &directory.join(&artifact.archive_path),
            &key.verifying_key(),
            &host,
        )
        .map_err(|error| error.to_string())?;
    }
    println!(
        "verified {} prepared plugin packages",
        index.artifacts.len()
    );
    Ok(())
}

fn stage_payloads(
    matrix: &BuiltinReleaseMatrix,
    staging: &Path,
) -> Result<BuiltinReleaseMatrix, String> {
    let mut staged = matrix.clone();
    for (target, binaries) in &mut staged.targets {
        let directory = staging.join(target);
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        for binary in binaries.values_mut() {
            let name = binary
                .file_name()
                .ok_or_else(|| format!("{}; expected executable filename", binary.display()))?;
            let destination = directory.join(name);
            fs::copy(&binary, &destination)
                .map_err(|error| format!("{}: {error}", binary.display()))?;
            *binary = destination;
        }
    }
    Ok(staged)
}

fn prepare_macos_plugins(
    matrix: &BuiltinReleaseMatrix,
    identity: Option<&str>,
    diagnostics: Option<&Path>,
) -> Result<(), String> {
    if identity.is_none() && diagnostics.is_none() {
        return Ok(());
    }
    for (target, binaries) in &matrix.targets {
        if !target.ends_with("-apple-darwin") {
            return Err(format!(
                "cannot Apple-sign target `{target}`; expected a macOS target"
            ));
        }
        for (id, binary) in binaries {
            let symbols = diagnostics
                .map(|root| {
                    NativeSymbols::capture(
                        binary,
                        &root.join("native/plugins").join(target).join(id),
                    )
                })
                .transpose()?;
            sign_macos_executable(binary, identity.unwrap_or("-"))?;
            if let Some(symbols) = symbols {
                symbols.record_signed_payload(binary)?;
            }
        }
    }
    Ok(())
}

fn sign_macos_executable(binary: &Path, identity: &str) -> Result<(), String> {
    let mut command = Command::new("codesign");
    command.args(["--force", "--sign", identity]);
    if identity != "-" {
        command.args(["--options", "runtime", "--timestamp"]);
    }
    let status = command.arg(binary).status().map_err(|error| {
        format!(
            "cannot sign `{}` with `{identity}`: {error}",
            binary.display()
        )
    })?;
    if !status.success() {
        return Err(format!(
            "codesign for `{}` returned {status}; expected successful signing with `{identity}`",
            binary.display()
        ));
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, fs};

    #[test]
    fn plugin_signing_is_optional_and_rejects_non_macos_targets() {
        let matrix = BuiltinReleaseMatrix {
            descriptors: BTreeMap::new(),
            manifests: BTreeMap::new(),
            targets: BTreeMap::from([("x86_64-unknown-linux-gnu".into(), BTreeMap::new())]),
        };
        assert!(prepare_macos_plugins(&matrix, None, None).is_ok());
        assert!(
            prepare_macos_plugins(&matrix, Some("-"), None)
                .unwrap_err()
                .contains("expected a macOS target")
        );
    }

    #[test]
    fn signs_real_executable_and_reports_missing_payload() {
        let directory = tempfile::tempdir().expect("signing fixture");
        let binary = directory.path().join("plugin with spaces");
        fs::copy(env::current_exe().expect("test binary"), &binary).expect("fixture copy");
        sign_macos_executable(&binary, "-").expect("ad-hoc signature");
        assert!(
            Command::new("codesign")
                .args(["--verify", "--strict"])
                .arg(&binary)
                .status()
                .unwrap()
                .success()
        );
        fs::remove_file(&binary).expect("missing fixture");
        assert!(
            sign_macos_executable(&binary, "-")
                .unwrap_err()
                .contains("expected successful signing")
        );
    }
}
