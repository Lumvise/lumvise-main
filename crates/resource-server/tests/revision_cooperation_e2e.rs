//! Real TLS/H2 clients in separate processes share only the server-owned database.
#[path = "revision_cooperation/client.rs"]
mod client;
#[path = "revision_cooperation/processes.rs"]
mod processes;
#[path = "revision_cooperation/scenarios.rs"]
mod scenarios;

use processes::{ProcessFixture, start_server};

#[test]
fn revision_network_process() {
    let Ok(role) = std::env::var("LUMVISE_REVISION_TEST_ROLE") else {
        return;
    };
    if role == "server" {
        processes::serve();
        return;
    }
    scenarios::run(&role);
}

fn run_client(root: &std::path::Path, endpoint: &str, role: &str) {
    let mut client = ProcessFixture::launch(root, role, Some(endpoint));
    client.expect("SCENARIO_DONE");
    client.finish();
}

fn simultaneous_writers(root: &std::path::Path, endpoint: &str, server_pid: u32) {
    let mut first = ProcessFixture::launch(root, "edit-a", Some(endpoint));
    let mut second = ProcessFixture::launch(root, "edit-b", Some(endpoint));
    assert_ne!(first.pid(), second.pid());
    assert_ne!(first.pid(), server_pid);
    assert_ne!(second.pid(), server_pid);
    first.expect("BASE_READY");
    second.expect("BASE_READY");
    first.signal("publish");
    second.signal("publish");
    first.expect("REPLY_DROPPED");
    second.expect("WRITE_ACCEPTED");
    first.finish();
    second.finish();
}

#[test]
fn separate_tls_clients_merge_scopes_replay_after_restart_and_preserve_required_history() {
    let root = tempfile::tempdir().unwrap();
    processes::create_certificate(root.path());
    let (server, endpoint) = start_server(root.path());
    run_client(root.path(), &endpoint, "seed");
    simultaneous_writers(root.path(), &endpoint, server.pid());
    server.finish();
    let (server, endpoint) = start_server(root.path());
    run_client(root.path(), &endpoint, "replay");
    run_client(root.path(), &endpoint, "artifact-history");
    server.finish();
    let (server, endpoint) = start_server(root.path());
    run_client(root.path(), &endpoint, "verify-collected");
    server.finish();
}
