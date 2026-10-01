use super::*;
use lumvise_plugin_runtime::PluginSystem;
use lumvise_plugin_semantic::CREATE_SNAPSHOT_EXPORT_ID;
use std::{path::Path, time::Instant};

fn seed_snapshot_project(runtime: &PluginSystem, root: &Path, name: &str) {
    success(
        runtime
            .invoke(
                PLUGIN_ID,
                INGEST_EXPORT_ID,
                serde_json::json!({
                    "provider_instance_id": "snapshot-test", "project_root": root,
                    "semantic_sources": [{"semantic_source_id": format!("{name}-source"),
                        "kind": "repository", "name": name, "root_path": root,
                        "root_uri": format!("file://{}", root.display())}],
                    "semantic_elements": [{"semantic_source_id": format!("{name}-source"),
                        "semantic_element_id": format!("{name}-file"), "path": "main.py",
                        "semantic_element_type": "file", "semantic_element_name": "main.py"}],
                    "semantic_relationships": []
                }),
            )
            .expect("seed snapshot project"),
    );
}

fn await_snapshot(runtime: &PluginSystem, operation_id: &str) -> serde_json::Value {
    let deadline = Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let status = success(
            runtime
                .invoke(
                    PLUGIN_ID,
                    SNAPSHOT_STATUS_EXPORT_ID,
                    serde_json::json!({"operation_id": operation_id}),
                )
                .expect("snapshot status"),
        );
        if status["status"] == "succeeded" {
            return status;
        }
        assert_ne!(status["status"], "failed", "snapshot failed: {status}");
        assert!(
            Instant::now() < deadline,
            "snapshot did not complete: {status}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn snapshot_project_selection_survives_the_signed_plugin_round_trip() {
    let workspace = tempfile::tempdir().expect("snapshot workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[51; 32]));
    let runtime = system(Arc::new(MemoryCapabilityBroker::default()));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let alpha = workspace.path().join("alpha");
    let micrograd = workspace.path().join("micrograd");
    seed_snapshot_project(&runtime, &alpha, "alpha");
    seed_snapshot_project(&runtime, &micrograd, "micrograd");
    let queued = success(
        runtime
            .invoke(
                PLUGIN_ID,
                CREATE_SNAPSHOT_EXPORT_ID,
                serde_json::json!({"project_root": micrograd}),
            )
            .expect("create selected snapshot"),
    );
    let completed = await_snapshot(&runtime, queued["operation_id"].as_str().unwrap());
    assert_eq!(
        completed["project_root"],
        micrograd.to_string_lossy().as_ref()
    );
    let exported = micrograd.join(".lumvise/graph_db.pz");
    assert_eq!(
        completed["result"]["outputPath"],
        exported.to_string_lossy().as_ref()
    );
    assert!(exported.is_file());
    assert!(!alpha.join(".lumvise/graph_db.pz").exists());
    runtime.stop(PLUGIN_ID).expect("stop");
}
