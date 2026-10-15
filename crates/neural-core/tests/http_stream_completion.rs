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

#[test]
fn streamed_text_can_call_a_blocking_tool_before_the_model_finishes() {
    let server = ChunkedProvider::start(b"first".to_vec(), b"tail".to_vec());
    let tool = ChunkedProvider::start(br#"{"accepted":true}"#.to_vec(), Vec::new());
    tool.release.send(()).unwrap();
    tool.release.send(()).unwrap();
    let tool_client = reqwest::blocking::Client::new();
    let mut delivered = Vec::new();
    ReqwestLlmHttpClient::new()
        .stream_text_until(&server.request(), &mut |chunk| {
            delivered.push(chunk.clone());
            if chunk == "tail" {
                return Ok(ControlFlow::Break(()));
            }
            let receipt: serde_json::Value = tool_client
                .post(&tool.endpoint)
                .json(&json!({"content":chunk,"final":false}))
                .send()
                .unwrap()
                .json()
                .unwrap();
            assert_eq!(receipt["accepted"], true);
            server.release.send(()).unwrap();
            Ok(ControlFlow::Continue(()))
        })
        .unwrap();
    assert_eq!(delivered, ["first", "tail"]);
    server.release.send(()).unwrap();
    server.thread.join().unwrap();
    tool.thread.join().unwrap();
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

#[test]
fn cancellation_interrupts_a_silent_http_body_without_waiting_for_eof() {
    let server = ChunkedProvider::start(b"first".to_vec(), b"tail".to_vec());
    let control = lumvise_resource_routing::InvocationControl::sixty_seconds();
    let cancel = control.clone();
    let (arrived, first) = mpsc::channel();
    let request = server.request();
    let call = std::thread::spawn(move || {
        ReqwestLlmHttpClient::new().stream_bytes_controlled(&request, &control, &mut |chunk| {
            arrived.send(chunk).unwrap();
            Ok(())
        })
    });
    assert_eq!(
        first.recv_timeout(Duration::from_secs(1)).unwrap(),
        b"first"
    );
    let started = std::time::Instant::now();
    cancel.cancel();
    assert!(matches!(
        call.join().unwrap(),
        Err(NeuralError::ProcessCancelled { .. })
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    // Release the server after cancellation proved independent of its remaining response.
    let _ = server.release.send(());
    let _ = server.release.send(());
    let _ = server.thread.join();
}

#[test]
fn speech_emits_pcm_before_the_provider_finishes_the_response() {
    use lumvise_neural_core::text2voice::{Text2VoiceRequest, Text2VoiceStreamEvent};
    use lumvise_neural_core::{
        OpenRouterSpeechConfig, OpenRouterSpeechSynthesizer, SpeechSynthesizer,
    };
    let server = ChunkedProvider::start(vec![1, 2], vec![3, 4]);
    let control = lumvise_resource_routing::InvocationControl::sixty_seconds();
    let voice = OpenRouterSpeechSynthesizer::new(
        OpenRouterSpeechConfig {
            endpoint: server.endpoint.clone(),
            credential: String::new(),
            model: "fake".into(),
            voice: None,
            timeout_ms: 60_000,
        },
        std::sync::Arc::new(ReqwestLlmHttpClient::new()),
    );
    let mut chunks = Vec::new();
    let result = voice.stream_with_events(
        &Text2VoiceRequest {
            text: "Hello".into(),
            voice_id: None,
            model: None,
        },
        &control,
        &mut |event| {
            if let Text2VoiceStreamEvent::AudioChunk { audio, .. } = event {
                chunks.push(audio);
                server.release.send(()).unwrap();
                if chunks.len() == 2 {
                    control.cancel();
                }
            }
            Ok(())
        },
    );
    assert!(result.is_err(), "test cancels before EOF");
    assert_eq!(chunks, [vec![1, 2], vec![3, 4]]);
    server.thread.join().unwrap();
}
