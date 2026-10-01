use lumvise_app_core::{AppCore, ScopedMcpHttpServer};
use lumvise_db_core::{
    LocalPersistence, RelationalPersistence, SemanticElement, SemanticOperation,
    SemanticPersistence, SemanticResult,
};
use lumvise_frontend_core::FrontendCore;
use lumvise_neural_core::LlmProviderRegistry;
use lumvise_resource_routing::InvocationControl;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn revision_ping_reconnects_then_resolves_graph_delta() {
    let db = Arc::new(LocalPersistence::in_memory().unwrap());
    sync_elements(db.as_ref(), vec![element("a"), element("b")]);
    let target_revision = semantic_revision(db.as_ref());
    let server = start_server(db);
    let address = loopback_address(&server);

    let initial = stream_response(&address, "/api/storage/changes/events", None);
    assert!(initial.contains(&format!("id: {target_revision}\nevent: storage_revision")));
    assert!(initial.contains(&format!("\"target_revision\":{target_revision}")));
    assert!(!initial.contains("storage_change"));

    let page = json_response(
        &address,
        "/api/storage/changes?project_root=/repo&after_revision=0&limit=10",
    );
    assert_eq!(page["base_revision"], 0);
    assert_eq!(page["target_revision"], target_revision);
    assert_eq!(page["head_revision"], target_revision);
    assert_eq!(page["has_more"], false);
    assert_eq!(
        page["changed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|changed| changed["element_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert!(
        page["changed"]
            .as_array()
            .unwrap()
            .iter()
            .all(|changed| changed["disposition"] == "upserted")
    );

    let reconnected = stream_response(
        &address,
        &format!("/api/storage/changes/events?after_revision={target_revision}"),
        None,
    );
    assert!(reconnected.contains("HTTP/1.1 200 OK"));
    assert!(!reconnected.contains("event: storage_revision"));
}

#[test]
fn cold_start_pull_rederives_from_graph() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let target_revision;
    {
        let db = LocalPersistence::open(file.path()).unwrap();
        sync_elements(&db, vec![element("cold")]);
        target_revision = semantic_revision(&db);
    }

    let server = start_server(Arc::new(LocalPersistence::open(file.path()).unwrap()));
    let page = json_response(
        &loopback_address(&server),
        "/api/storage/changes?project_root=/repo&after_revision=0&limit=10",
    );
    assert_eq!(page["target_revision"], target_revision);
    assert_eq!(page["has_more"], false);
    assert_eq!(page["changed"][0]["element_id"], "cold");
}

#[test]
fn ahead_revision_cursor_emits_error_then_closes() {
    let db = Arc::new(LocalPersistence::in_memory().unwrap());
    sync_elements(db.as_ref(), vec![element("ahead")]);
    let ahead = semantic_revision(db.as_ref()) + 1;
    let server = start_server(db);
    let address = loopback_address(&server);

    let response = stream_response(
        &address,
        &format!("/api/storage/changes/events?after_revision={ahead}"),
        None,
    );
    assert!(response.contains("HTTP/1.1 200 OK"));
    assert!(response.contains("event: cursor_error"));
    assert!(response.contains("\"reason\":\"ahead\""));

    let pull = stream_response(
        &address,
        &format!("/api/storage/changes?project_root=/repo&after_revision={ahead}"),
        None,
    );
    assert!(pull.starts_with("HTTP/1.1 400 Bad Request"), "{pull}");
}

fn element(id: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".into(),
        semantic_element_id: id.into(),
        semantic_source_id: "source".into(),
        path: format!("src/{id}.rs"),
        element_kind: "file".into(),
        name: id.into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: serde_json::json!({}),
    }
}

fn sync_elements(db: &LocalPersistence, elements: Vec<SemanticElement>) {
    <LocalPersistence as SemanticPersistence>::execute(
        db,
        SemanticOperation::SyncStructure {
            project_root: "/repo".into(),
            elements,
            relationships: Vec::new(),
        },
        &InvocationControl::sixty_seconds(),
    )
    .expect("sync semantic elements");
}

fn semantic_revision(db: &LocalPersistence) -> i64 {
    match <LocalPersistence as SemanticPersistence>::execute(
        db,
        SemanticOperation::SemanticRevision,
        &InvocationControl::sixty_seconds(),
    )
    .expect("read semantic revision")
    {
        SemanticResult::SemanticRevision { commit_version } => commit_version,
        result => panic!("unexpected semantic revision result: {result:?}"),
    }
}

fn start_server(persistence: Arc<LocalPersistence>) -> ScopedMcpHttpServer {
    let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
    let relational: Arc<dyn RelationalPersistence> = persistence;
    let app = Arc::new(AppCore::new(
        semantic,
        relational,
        FrontendCore::default(),
        LlmProviderRegistry::empty(),
    ));
    ScopedMcpHttpServer::spawn(app).unwrap()
}

fn loopback_address(server: &ScopedMcpHttpServer) -> String {
    server
        .base_url()
        .strip_prefix("http://")
        .expect("loopback URL")
        .to_owned()
}

fn stream_response(address: &str, path: &str, extra_headers: Option<&str>) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\n{}\r\n",
        extra_headers.unwrap_or_default()
    )
    .unwrap();
    read_response(&mut stream)
}

fn json_response(address: &str, path: &str) -> Value {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let response = read_response(&mut stream);
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    let body = response.split_once("\r\n\r\n").unwrap().1;
    serde_json::from_str(body).unwrap()
}

fn read_response(stream: &mut TcpStream) -> String {
    let mut response = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => response.extend_from_slice(&chunk[..read]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(error) => panic!("read HTTP response: {error}"),
        }
    }
    String::from_utf8(response).unwrap()
}
