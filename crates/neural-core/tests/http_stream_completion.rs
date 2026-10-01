use lumvise_neural_core::NeuralError;
use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
use lumvise_neural_core::llm_providers::http_client::ReqwestLlmHttpClient;
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::ops::ControlFlow;
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

struct ChunkedProvider {
    endpoint: String,
    release: Sender<()>,
    thread: JoinHandle<()>,
}

impl ChunkedProvider {
    fn start(first: Vec<u8>, second: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (release, next) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(&stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
            for chunk in [first, second] {
                write!(stream, "{:x}\r\n", chunk.len()).unwrap();
                stream.write_all(&chunk).unwrap();
                stream.write_all(b"\r\n").unwrap();
                stream.flush().unwrap();
                next.recv_timeout(Duration::from_secs(3))
                    .expect("client waited for the unread response tail");
            }
        });
        Self {
            endpoint,
            release,
            thread,
        }
    }

    fn request(&self) -> LlmHttpRequest {
        LlmHttpRequest {
            timeout: None,
            endpoint: self.endpoint.clone(),
            credential: "fake".into(),
            headers: BTreeMap::new(),
            payload: json!({}),
        }
    }
}

#[test]
fn controlled_http_stream_preserves_split_unicode_and_returns_before_response_eof() {
    let server = ChunkedProvider::start(vec![b'a', 0xf0], vec![0x9f, 0x90, 0xbc, b'!']);
    let mut observed = Vec::new();
    ReqwestLlmHttpClient::new()
        .stream_text_until(&server.request(), &mut |chunk| {
            observed.push(chunk);
            if observed.len() == 1 {
                server.release.send(()).unwrap();
                return Ok(ControlFlow::Continue(()));
            }
            Ok(ControlFlow::Break(()))
        })
        .unwrap();
    assert_eq!(observed, vec!["a", "🐼!"]);
    server.release.send(()).unwrap();
    server.thread.join().unwrap();
}

// An exhausted caller deadline must fail before any bytes are sent, so the
// provider lane frees immediately instead of waiting on the 180s ceiling.

#[test]
fn zero_timeout_fails_with_process_timeout_before_sending() {
    // Bound endpoint that would hang forever if contacted; the zero timeout
    // must short-circuit before any connection is attempted.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let request = LlmHttpRequest {
        timeout: Some(Duration::ZERO),
        endpoint,
        credential: "fake".into(),
        headers: BTreeMap::new(),
        payload: json!({}),
    };

    let error = ReqwestLlmHttpClient::new().post_json(&request).unwrap_err();

    assert!(matches!(error, NeuralError::ProcessTimeout { .. }));
    // Nothing connected: the timeout short-circuits before the send.
    listener.set_nonblocking(true).unwrap();
    assert!(
        listener.accept().is_err(),
        "no connection should have been attempted"
    );
}
