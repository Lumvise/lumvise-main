//! Exercise the shipped CLI with no desktop assets or installed plugins.
#![cfg(not(feature = "desktop-app"))]

use lumvise_app_core::{AcquireResult, ActivationRequest, AppRuntimeCoordinator};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct CommunityProcess(Child);

impl Drop for CommunityProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn isolated_command(root: &Path) -> Command {
    let mut command = Command::new(root.join(format!("lumvise{}", std::env::consts::EXE_SUFFIX)));
    // Provider discovery must not use the developer's credentials or CLI sessions.
    command.env_clear();
    command
        .env("LUMVISE_RUNTIME_ROOT", root.join("runtime"))
        .env("LUMVISE_DB_PATH", root.join("database"))
        .env("LUMVISE_PLUGIN_ROOT", root.join("plugins"))
        .env("LUMVISE_STATE_ROOT", root.join("state"))
        .env(
            "LUMVISE_PROVIDER_MODEL_CATALOG_PATH",
            root.join("models.json"),
        )
        .env_remove("LUMVISE_BUILTIN_RELEASE_DIR")
        .env("LUMVISE_METRICS_INTERVAL_SECS", "0")
        .env("LUMVISE_LOG_FORMAT", "json")
        .env("RUST_BACKTRACE", "1")
        .stdout(Stdio::null());
    for provider in ["CODEX", "CLAUDE", "GEMINI"] {
        command.env(
            format!("LUMVISE_{provider}_COMMAND"),
            root.join("no-provider-cli"),
        );
    }
    for route in [
        "LLM_EXECUTION",
        "SPEECH_INFERENCE",
        "GRAPH_PERSISTENCE",
        "SQL_PERSISTENCE",
    ] {
        command.env(format!("LUMVISE_ROUTE_{route}"), "internal");
    }
    command
}

fn wait_until_ready(process: &mut CommunityProcess, coordinator: &AppRuntimeCoordinator) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            process.0.try_wait().unwrap().is_none(),
            "community host exited before ready"
        );
        if coordinator.try_ready_connection().unwrap().is_some() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "community host failed to become ready"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn tools_over_stdio(root: &Path) -> Value {
    let mut child = CommunityProcess(
        isolated_command(root)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let output = child.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut input = child.0.stdin.take().unwrap();
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"community-test","version":"1"}}})).unwrap();
    let response: Value =
        serde_json::from_str(&receiver.recv_timeout(Duration::from_secs(10)).unwrap()).unwrap();
    assert!(response.get("result").is_some(), "initialize: {response}");
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})
    )
    .unwrap();
    let response: Value =
        serde_json::from_str(&receiver.recv_timeout(Duration::from_secs(10)).unwrap()).unwrap();
    drop(input);
    drop(child);
    reader.join().unwrap();
    response
}

#[test]
fn community_cli_starts_without_commercial_plugins_and_serves_mcp() {
    let workspace = tempfile::tempdir().unwrap();
    // The developer target directory can contain a bundled commercial release.
    // Copy only the executable to prove neither that release nor source is needed.
    std::fs::copy(
        env!("CARGO_BIN_EXE_lumvise"),
        workspace
            .path()
            .join(format!("lumvise{}", std::env::consts::EXE_SUFFIX)),
    )
    .unwrap();
    let mut process = CommunityProcess(isolated_command(workspace.path()).spawn().unwrap());
    let coordinator = AppRuntimeCoordinator::new(workspace.path().join("runtime"), |_| {
        panic!("already started community host must not launch another process")
    });
    wait_until_ready(&mut process, &coordinator);
    assert!(matches!(
        coordinator
            .acquire_or_forward(ActivationRequest::default())
            .unwrap(),
        AcquireResult::Forwarded(_)
    ));
    let response = tools_over_stdio(workspace.path());
    let tools = response["result"]["tools"]
        .as_array()
        .expect("MCP tool catalog");
    assert!(
        !tools
            .iter()
            .any(|tool| tool["name"].as_str().unwrap().contains("builtin.assistant"))
    );
    assert!(
        workspace
            .path()
            .join("plugins/publisher-trust.json")
            .is_file()
    );
    #[cfg(unix)]
    assert_orderly_termination(&mut process, workspace.path());
}

#[cfg(unix)]
fn assert_orderly_termination(process: &mut CommunityProcess, root: &Path) {
    assert!(
        Command::new("kill")
            .args(["-TERM", &process.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = process.0.try_wait().unwrap() {
            assert!(status.success(), "community shutdown failed: {status}");
            assert!(!root.join("runtime/runtime.json").exists());
            return;
        }
        assert!(Instant::now() < deadline, "community runtime did not stop");
        std::thread::sleep(Duration::from_millis(30));
    }
}
