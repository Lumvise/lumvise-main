use serde_json::Value;
use std::process::Command;

#[test]
fn workspace_dependency_graph_contains_no_legacy_plugin_packages() {
    let metadata = workspace_metadata();
    let package_names = workspace_package_names(&metadata);

    assert!(
        !package_names
            .iter()
            .any(|name| *name == "lumvise-plugin-core")
    );
    assert!(!package_names.iter().any(|name| *name == "lumvise-plugins"));
}

fn workspace_metadata() -> Value {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .expect("cargo metadata executable");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("cargo metadata JSON")
}

fn workspace_package_names(metadata: &Value) -> Vec<&str> {
    let member_ids = metadata["workspace_members"]
        .as_array()
        .expect("workspace members");
    metadata["packages"]
        .as_array()
        .expect("workspace packages")
        .iter()
        .filter(|package| member_ids.contains(&package["id"]))
        .filter_map(|package| package["name"].as_str())
        .collect()
}
