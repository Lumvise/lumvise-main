use serde_json::Value;
use std::process::Command;

const ALLOWED_DEPENDENCIES: &[&str] = &[
    "lumvise-app-core",
    "lumvise-contracts",
    "lumvise-mcp-core",
    "prost",
    "serde",
    "serde_json",
    "tempfile",
    "thiserror",
    "tokio",
    "tracing",
];

#[test]
fn adapter_dependency_graph_is_transport_only() {
    let dependencies = package_dependencies(&cargo_metadata(), "lumvise-mcp-app-adapter");

    assert_allowed_dependencies(&dependencies).unwrap();
}

#[test]
fn runtime_launch_and_mcp_wire_contracts_are_allowed() {
    let dependencies = vec![
        "lumvise-app-core".to_string(),
        "lumvise-contracts".to_string(),
    ];
    assert_allowed_dependencies(&dependencies).unwrap();
}

#[test]
fn mcp_wire_contracts_do_not_admit_domain_implementations() {
    for dependency in [
        "lumvise-db-core",
        "lumvise-neural-core",
        "lumvise-plugin-semantic",
    ] {
        let error = assert_allowed_dependencies(&[dependency.to_string()]).unwrap_err();
        assert!(error.contains(dependency));
    }
}

fn assert_allowed_dependencies(dependencies: &[String]) -> Result<(), String> {
    for dependency in dependencies {
        if !ALLOWED_DEPENDENCIES.contains(&dependency.as_str()) {
            return Err(format!("forbidden adapter dependency `{dependency}`"));
        }
    }
    Ok(())
}

fn cargo_metadata() -> Value {
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

fn package_dependencies(metadata: &Value, package_name: &str) -> Vec<String> {
    let packages = metadata["packages"].as_array().expect("workspace packages");
    let package = packages
        .iter()
        .find(|package| package["name"] == package_name)
        .unwrap_or_else(|| panic!("missing package `{package_name}` in cargo metadata"));
    package["dependencies"]
        .as_array()
        .expect("package dependencies")
        .iter()
        .filter_map(|dependency| dependency["name"].as_str().map(str::to_owned))
        .collect()
}
