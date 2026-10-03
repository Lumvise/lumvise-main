use super::*;
use std::collections::BTreeMap;

struct FakeModelEnvironment(BTreeMap<String, OsString>);

impl FakeModelEnvironment {
    fn read(&self, name: &str) -> Option<OsString> {
        self.0.get(name).cloned()
    }
}

#[test]
fn model_environment_excludes_unrelated_variables_and_plugins() {
    let environment = FakeModelEnvironment(BTreeMap::from([
        ("DOCLING_LAYOUT_ONNX".into(), "/missing/layout.onnx".into()),
        ("PROVIDER_API_KEY".into(), "must-not-cross".into()),
    ]));
    let assets = DocumentModelAssets::for_plugin("builtin.canvas", |name| environment.read(name));
    let mut command = Command::new("/usr/bin/sandbox-exec");
    assets.apply(command.env_clear(), "(version 1)");
    assert_eq!(command.get_envs().count(), 1);
    assert_eq!(command.get_envs().next().unwrap().0, "DOCLING_LAYOUT_ONNX");
    assert!(assets.read_directories.is_empty());
    let other = DocumentModelAssets::for_plugin("builtin.semantic", |name| environment.read(name));
    assert!(other.environment.is_empty());
}

#[test]
fn model_assets_grant_sidecars_and_library_directory_without_profile_interpolation() {
    let root = tempfile::tempdir().unwrap();
    let model = root.path().join("layout.onnx");
    std::fs::write(&model, "graph").unwrap();
    let environment = FakeModelEnvironment(BTreeMap::from([
        ("DOCLING_LAYOUT_ONNX".into(), model.into_os_string()),
        (
            "PDFIUM_DYNAMIC_LIB_PATH".into(),
            root.path().as_os_str().to_owned(),
        ),
    ]));
    let assets = DocumentModelAssets::for_plugin("builtin.canvas", |name| environment.read(name));
    let mut command = Command::new("/usr/bin/sandbox-exec");
    assets.apply(command.env_clear(), "(version 1)\n(deny default)");
    assert_eq!(assets.read_directories.len(), 1);
    let arguments: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy())
        .collect();
    assert!(
        arguments
            .last()
            .unwrap()
            .contains("(subpath (param \"DOCUMENT_ASSET_ROOT_0\"))")
    );
    assert!(
        !arguments
            .last()
            .unwrap()
            .contains(root.path().to_str().unwrap())
    );
}

#[test]
fn sandbox_reads_configured_graph_sidecar_but_denies_unrelated_files() {
    let root = tempfile::tempdir().unwrap();
    let model_root = root.path().join("models");
    std::fs::create_dir(&model_root).unwrap();
    let model = model_root.join("layout.onnx");
    let sidecar = model_root.join("layout.onnx.data");
    let unrelated = root.path().join("unrelated.txt");
    for path in [&model, &sidecar, &unrelated] {
        std::fs::write(path, "asset").unwrap();
    }
    let environment = FakeModelEnvironment(BTreeMap::from([(
        "DOCLING_LAYOUT_ONNX".into(),
        model.clone().into_os_string(),
    )]));
    let assets = DocumentModelAssets::for_plugin("builtin.canvas", |name| environment.read(name));
    assert!(sandbox_read(&assets, &model).success());
    assert!(sandbox_read(&assets, &sidecar).success());
    assert!(!sandbox_read(&assets, &unrelated).success());
}

fn sandbox_read(assets: &DocumentModelAssets, path: &std::path::Path) -> std::process::ExitStatus {
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command
        .env_clear()
        .arg("-D")
        .arg("PACKAGE_ROOT=/nonexistent-document-test-package")
        .arg("-D")
        .arg("EXECUTION_ROOT=/bin");
    assets.apply(&mut command, super::super::MACOS_SANDBOX_PROFILE);
    command.arg("/bin/cat").arg(path).output().unwrap().status
}
