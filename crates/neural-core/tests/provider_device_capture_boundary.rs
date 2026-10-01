use lumvise_neural_core::llm_providers::{LlmModalityInput, LlmModalityInputKind};
use serde_json::{Value, json};
use std::process::Command;

const DEVICE_CAPTURE_CRATES: &[&str] = &["cpal", "scrap", "xcap", "screencapturekit"];

#[test]
fn neural_core_dependency_graph_has_no_device_capture_implementation() {
    let dependencies = package_dependencies(&cargo_metadata(), "lumvise-neural-core");

    for dependency in dependencies {
        assert!(
            !DEVICE_CAPTURE_CRATES.contains(&dependency.as_str()),
            "neural-core depends on device capture crate `{dependency}`"
        );
    }
}

#[test]
fn provider_boundary_accepts_host_captured_bytes() {
    let input = LlmModalityInput {
        input_id: "audio-1".into(),
        kind: LlmModalityInputKind::LiveAudioChunk,
        media_type: "audio/pcm".into(),
        bytes: vec![1, 2, 3],
        metadata: json!({"sequence": 1}),
    };

    let encoded = serde_json::to_value(input).unwrap();
    assert_eq!(encoded["bytes"], json!([1, 2, 3]));
    assert_eq!(encoded["kind"], json!("live_audio_chunk"));
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
