//! Revision-notification Server-Sent Events for semantic graph changes.
//!
//! Events are level-triggered wakes. Clients retain an applied commit revision
//! and resolve `(applied_revision, target_revision]` through the separate
//! graph-derived changes-since-revision read.

use super::mcp_http::{
    HttpRequest, HttpResponse, error_response, json_response, write_async_http_response,
};
use crate::AppCore;
use lumvise_db_core::{ChangeHookScope, SemanticOperation, SemanticResult};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;

pub(crate) const STORAGE_CHANGE_STREAM_ENDPOINT: &str = "/api/storage/changes/events";
pub(crate) const STORAGE_CHANGES_ENDPOINT: &str = "/api/storage/changes";
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 1_000;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy)]
struct Subscription {
    after_revision: Option<i64>,
}

impl Subscription {
    fn parse(request: &HttpRequest) -> Result<Self, String> {
        let after_revision = request
            .query
            .get("after_revision")
            .map(|value| parse_revision(value))
            .transpose()?;
        Ok(Self { after_revision })
    }
}

fn parse_revision(value: &str) -> Result<i64, String> {
    let parsed = value
        .parse::<i64>()
        .map_err(|_| "after_revision must be a non-negative decimal i64".to_string())?;
    if parsed < 0 {
        return Err("after_revision must be a non-negative decimal i64".to_string());
    }
    Ok(parsed)
}

/// Handles the admitted, buffered graph-derived changes-since-revision read.
pub(crate) fn changes_since_response(app: &AppCore, request: &HttpRequest) -> HttpResponse {
    let scope = match scope_from_request(request) {
        Ok(scope) => scope,
        Err(error) => return error_response("400 Bad Request", error),
    };
    let after_revision = match request.query.get("after_revision") {
        Some(value) => match parse_revision(value) {
            Ok(revision) => revision,
            Err(error) => return error_response("400 Bad Request", error),
        },
        None => return error_response("400 Bad Request", "after_revision is required"),
    };
    let limit = match limit_from_request(request) {
        Ok(limit) => limit,
        Err(error) => return error_response("400 Bad Request", error),
    };
    match app
        .database()
        .changes_since_revision(scope, after_revision, limit)
    {
        Ok(page) => json_response(
            "200 OK",
            serde_json::to_value(page).expect("page is serializable"),
        ),
        Err(error) => error_response("400 Bad Request", error.to_string()),
    }
}

fn scope_from_request(request: &HttpRequest) -> Result<ChangeHookScope, String> {
    let project_root = request
        .query
        .get("project_root")
        .map(String::as_str)
        .filter(|root| !root.trim().is_empty())
        .ok_or_else(|| "project_root is required".to_string())?;
    let entity_kinds = request
        .query
        .get("entity_kinds")
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .map(|kind| {
                    if kind.is_empty() {
                        Err("entity_kinds must not contain an empty kind".to_string())
                    } else {
                        Ok(kind.to_string())
                    }
                })
                .collect::<Result<BTreeSet<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(ChangeHookScope {
        project_root: project_root.to_string(),
        entity_kinds,
    })
}

fn limit_from_request(request: &HttpRequest) -> Result<usize, String> {
    let limit = request
        .query
        .get("limit")
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| "limit must be an integer from 1 through 1000".to_string())
        })
        .transpose()?
        .unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err("limit must be an integer from 1 through 1000".to_string());
    }
    Ok(limit)
}

/// Serves one revision-notification subscription without entering normal HTTP admission.
pub(crate) async fn serve(
    app: Arc<AppCore>,
    mut stream: TcpStream,
    request: HttpRequest,
    mut shutdown: watch::Receiver<bool>,
) {
    let subscription = match Subscription::parse(&request) {
        Ok(subscription) => subscription,
        Err(error) => {
            let _ =
                write_async_http_response(&mut stream, error_response("400 Bad Request", error))
                    .await;
            return;
        }
    };
    let head = match semantic_revision(&app).await {
        Ok(head) => head,
        Err(error) => {
            let _ = write_async_http_response(
                &mut stream,
                error_response("500 Internal Server Error", error),
            )
            .await;
            return;
        }
    };
    let mut observed_revision = subscription.after_revision.unwrap_or(0);
    if observed_revision > head {
        if write_stream_head(&mut stream).await.is_ok() {
            let _ = write_ahead_error(&mut stream, observed_revision, head).await;
        }
        return;
    }
    if write_stream_head(&mut stream).await.is_err() {
        return;
    }
    if head > observed_revision && write_revision_ping(&mut stream, head).await.is_err() {
        return;
    }
    observed_revision = head.max(observed_revision);

    let (mut read_half, mut write_half) = stream.into_split();
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.tick().await;
    let mut client_byte = [0_u8; 1];
    loop {
        tokio::select! {
            shutdown_changed = shutdown.changed() => {
                if shutdown_changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            client_read = read_half.read(&mut client_byte) => {
                if !matches!(client_read, Ok(bytes) if bytes > 0) {
                    return;
                }
            }
            revision = wait_for_semantic_revision(&app, observed_revision) => {
                match revision {
                    Ok(head) if head > observed_revision => {
                        if write_revision_ping(&mut write_half, head).await.is_err() {
                            return;
                        }
                        observed_revision = head;
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
            _ = heartbeat.tick() => {
                if write_half.write_all(b": heartbeat\n\n").await.is_err()
                    || write_half.flush().await.is_err()
                {
                    return;
                }
            }
        }
    }
}

async fn semantic_revision(app: &Arc<AppCore>) -> Result<i64, String> {
    let semantic = Arc::clone(&app.semantic);
    tokio::task::spawn_blocking(move || {
        match semantic.execute(
            SemanticOperation::SemanticRevision,
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::SemanticRevision { commit_version } => Ok(commit_version),
            result => Err(lumvise_db_core::DbError::invalid_value(
                format!("{result:?}"),
                "semantic revision result",
            )),
        }
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

async fn wait_for_semantic_revision(
    app: &Arc<AppCore>,
    after_revision: i64,
) -> Result<i64, String> {
    let semantic = Arc::clone(&app.semantic);
    tokio::task::spawn_blocking(move || {
        match semantic.execute(
            SemanticOperation::WaitForSemanticRevision {
                after_revision,
                timeout_ms: Some(HEARTBEAT_INTERVAL.as_millis() as u64),
            },
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::SemanticRevision { commit_version } => Ok(commit_version),
            result => Err(lumvise_db_core::DbError::invalid_value(
                format!("{result:?}"),
                "semantic revision wait result",
            )),
        }
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

async fn write_stream_head(stream: &mut (impl AsyncWrite + Unpin)) -> std::io::Result<()> {
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
        )
        .await?;
    stream.flush().await
}

async fn write_revision_ping(
    stream: &mut (impl AsyncWrite + Unpin),
    target_revision: i64,
) -> std::io::Result<()> {
    let data = json!({
        "schema_version": 1,
        "target_revision": target_revision,
    });
    let frame = format!("id: {target_revision}\nevent: storage_revision\ndata: {data}\n\n");
    stream.write_all(frame.as_bytes()).await?;
    stream.flush().await
}

async fn write_ahead_error(
    stream: &mut (impl AsyncWrite + Unpin),
    requested_after_revision: i64,
    target_revision: i64,
) -> std::io::Result<()> {
    let data = json!({
        "schema_version": 1,
        "reason": "ahead",
        "requested_after_revision": requested_after_revision,
        "target_revision": target_revision,
    });
    let frame = format!("event: cursor_error\ndata: {data}\n\n");
    stream.write_all(frame.as_bytes()).await?;
    stream.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumvise_db_core::{SemanticElement, SemanticRelationship};
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

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
            metadata: json!({}),
        }
    }

    fn sync_element(app: &AppCore, id: &str) {
        app.semantic
            .execute(
                SemanticOperation::SyncStructure {
                    project_root: "/repo".into(),
                    elements: vec![element(id)],
                    relationships: Vec::<SemanticRelationship>::new(),
                },
                &InvocationControl::sixty_seconds(),
            )
            .unwrap();
    }

    #[tokio::test]
    async fn committed_revision_wakes_stream_promptly_and_shutdown_ends_handler() {
        let app = Arc::new(AppCore::in_memory().unwrap());
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(address).await.unwrap();
        let (server_socket, _) = listener.accept().await.unwrap();
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let handler = tokio::spawn(serve(
            Arc::clone(&app),
            server_socket,
            HttpRequest {
                method: "GET".into(),
                path: STORAGE_CHANGE_STREAM_ENDPOINT.into(),
                query: Default::default(),
                body: Vec::new(),
                authorization: None,
            },
            shutdown_receiver,
        ));

        let mut buffer = [0_u8; 2048];
        let initial = tokio::time::timeout(Duration::from_millis(300), client.read(&mut buffer))
            .await
            .expect("stream response promptly")
            .unwrap();
        assert!(String::from_utf8_lossy(&buffer[..initial]).contains("200 OK"));

        sync_element(&app, "committed-wake");
        let wake = tokio::time::timeout(Duration::from_millis(500), client.read(&mut buffer))
            .await
            .expect("committed revision wake must not wait for heartbeat")
            .unwrap();
        let response = String::from_utf8_lossy(&buffer[..wake]);
        assert!(response.contains("id: 1\nevent: storage_revision"));
        assert!(response.contains("\"target_revision\":1"));

        shutdown_sender.send(true).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(500), client.read(&mut buffer))
                .await
                .expect("shutdown closes client socket")
                .unwrap(),
            0
        );
        handler.await.unwrap();
    }

    #[test]
    fn subscription_parsing_rejects_invalid_revision_values() {
        let request = HttpRequest {
            method: "GET".into(),
            path: STORAGE_CHANGE_STREAM_ENDPOINT.into(),
            query: [("after_revision".into(), "7".into())].into(),
            body: Vec::new(),
            authorization: None,
        };
        assert_eq!(
            Subscription::parse(&request).unwrap().after_revision,
            Some(7)
        );

        let invalid = HttpRequest {
            query: [("after_revision".into(), "-1".into())].into(),
            ..request
        };
        assert!(Subscription::parse(&invalid).is_err());
    }

    #[test]
    fn changes_since_request_requires_a_scope_and_valid_limit() {
        let request = HttpRequest {
            method: "GET".into(),
            path: STORAGE_CHANGES_ENDPOINT.into(),
            query: [("after_revision".into(), "0".into())].into(),
            body: Vec::new(),
            authorization: None,
        };
        assert!(scope_from_request(&request).is_err());
        let invalid_limit = HttpRequest {
            query: [
                ("project_root".into(), "/repo".into()),
                ("after_revision".into(), "0".into()),
                ("limit".into(), "0".into()),
            ]
            .into(),
            ..request
        };
        assert!(limit_from_request(&invalid_limit).is_err());
    }
}
