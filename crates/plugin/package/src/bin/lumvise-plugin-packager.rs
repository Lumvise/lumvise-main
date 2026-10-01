use std::{env, fs, path::Path};

use lumvise_plugin_package::{
    PluginManifest, build_package_from_directory, read_protected_signing_key,
};

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("plugin package build failed: {error}");
        std::process::exit(2);
    }
}

fn run(arguments: Vec<String>) -> Result<(), String> {
    let [manifest_path, payload_root, key_path, output_path] = arguments.as_slice() else {
        return Err(
            "expected: lumvise-plugin-packager <manifest.json> <payload-root> <signing-key> <output.lvp>"
                .into(),
        );
    };
    let manifest = read_manifest(Path::new(manifest_path))?;
    let signing_key =
        read_protected_signing_key(Path::new(key_path)).map_err(|error| error.to_string())?;
    build_package_from_directory(
        Path::new(output_path),
        manifest,
        Path::new(payload_root),
        &signing_key,
    )
    .map_err(|error| error.to_string())?;
    println!("created {output_path}");
    Ok(())
}

fn read_manifest(path: &Path) -> Result<PluginManifest, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("cannot read manifest `{}`: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid manifest `{}`: {error}", path.display()))
}
