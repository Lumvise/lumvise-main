use std::collections::VecDeque;
use std::ops::ControlFlow;
use std::sync::{Arc, Mutex};

use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
use lumvise_neural_core::text2voice::Text2VoiceRequest;
use lumvise_neural_core::voice2text::Voice2TextRequest;
use lumvise_neural_core::{
    BearerTokenSource, NeuralError, OpenRouterSpeechConfig, OpenRouterSpeechRecognizer,
    OpenRouterSpeechSynthesizer, Result, SpeechRecognizer, SpeechSynthesizer, TokenInjectingClient,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};

const SIGN_IN_MESSAGE: &str = "Sign in to your Lumvise account.";
const BALANCE_MESSAGE: &str = "Lumvise token balance is used up.";
const DAILY_LIMIT_MESSAGE: &str = "Daily Lumvise cloud limit reached.";

struct Reply {
    chunks: Vec<String>,
    outcome: Result<Value>,
}

fn reply(outcome: Result<Value>) -> Reply {
    Reply {
        chunks: Vec::new(),
        outcome,
    }
}

fn http_failure(message: &str) -> Result<Value> {
    Err(NeuralError::ProviderFailed {
        provider_id: "https://app.lumvise.com/api/public/v1/ai/audio/speech".into(),
        message: message.into(),
    })
}

/// Records every request and answers from a script, one reply per call.
#[derive(Default)]
struct FakeInner {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<LlmHttpRequest>>,
}

impl FakeInner {
    fn scripted(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            requests: Mutex::default(),
        })
    }

    fn next(&self, request: &LlmHttpRequest) -> Reply {
        self.requests.lock().unwrap().push(request.clone());
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted reply")
    }

    fn credentials(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.credential.clone())
            .collect()
    }
}

impl LlmHttpClient for FakeInner {
    fn post_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        self.next(request).outcome
    }

    fn post_bytes(&self, request: &LlmHttpRequest) -> Result<Vec<u8>> {
        self.next(request).outcome.map(|_| b"pcm".to_vec())
    }

    fn get_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        self.next(request).outcome
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> Result<Vec<String>> {
        let reply = self.next(request);
        reply.outcome.map(|_| reply.chunks)
    }

    fn stream_text_until(
        &self,
        request: &LlmHttpRequest,
        on_chunk: &mut dyn FnMut(String) -> Result<ControlFlow<()>>,
    ) -> Result<()> {
        let reply = self.next(request);
        for chunk in reply.chunks {
            if on_chunk(chunk)?.is_break() {
                break;
            }
        }
        reply.outcome.map(|_| ())
    }
}

/// Answers "cached" normally and "renewed" after a forced refresh; logs each `force_refresh`.
#[derive(Default)]
struct FakeTokens {
    log: Mutex<Vec<bool>>,
    signed_out: bool,
}

impl BearerTokenSource for FakeTokens {
    fn bearer_token(
        &self,
        force_refresh: bool,
        _control: &lumvise_resource_routing::InvocationControl,
    ) -> std::result::Result<String, String> {
        self.log.lock().unwrap().push(force_refresh);
        if self.signed_out {
            return Err(SIGN_IN_MESSAGE.into());
        }
        Ok(if force_refresh { "renewed" } else { "cached" }.into())
    }
}

fn client(inner: &Arc<FakeInner>, tokens: &Arc<FakeTokens>) -> TokenInjectingClient {
    TokenInjectingClient::new(
        Arc::clone(inner) as Arc<dyn LlmHttpClient>,
        Arc::clone(tokens) as Arc<dyn BearerTokenSource>,
    )
}

fn request() -> LlmHttpRequest {
    LlmHttpRequest {
        timeout: None,
        endpoint: "https://app.lumvise.com/api/public/v1/ai/audio/speech".into(),
        credential: String::new(),
        headers: Default::default(),
        payload: json!({}),
    }
}

struct ExpiringTokens;

impl BearerTokenSource for ExpiringTokens {
    fn bearer_token(
        &self,
        _: bool,
        control: &lumvise_resource_routing::InvocationControl,
    ) -> std::result::Result<String, String> {
        std::thread::sleep(control.remaining() + std::time::Duration::from_millis(1));
        Ok("late token".into())
    }
}

#[test]
fn expired_credential_lookup_never_dispatches_http_with_a_stale_timeout() {
    let inner = FakeInner::scripted(Vec::new());
    let client = TokenInjectingClient::new(inner.clone(), Arc::new(ExpiringTokens));
    let control = lumvise_resource_routing::InvocationControl::with_deadline(
        std::time::Duration::from_millis(10),
    );
    let result = client.post_json_controlled(&request(), &control);
    assert!(matches!(result, Err(NeuralError::ProcessTimeout { .. })));
    assert!(inner.requests.lock().unwrap().is_empty());
}

fn message(error: NeuralError) -> (String, String) {
    match error {
        NeuralError::ProviderFailed {
            provider_id,
            message,
        } => (provider_id, message),
        other => panic!("unexpected error kind: {other}"),
    }
}

#[test]
fn injects_a_fresh_token_into_json_and_binary_posts() {
    let inner = FakeInner::scripted(vec![reply(Ok(json!({}))), reply(Ok(json!({})))]);
    let client = client(&inner, &Arc::default());

    client.post_json(&request()).unwrap();
    assert_eq!(client.post_bytes(&request()).unwrap(), b"pcm");

    assert_eq!(inner.credentials(), ["cached", "cached"]);
}

#[test]
fn signed_out_fails_with_sign_in_message_without_calling_inner() {
    let inner = FakeInner::scripted(Vec::new());
    let signed_out = Arc::new(FakeTokens {
        signed_out: true,
        ..FakeTokens::default()
    });

    let (provider, text) = message(
        client(&inner, &signed_out)
            .post_json(&request())
            .unwrap_err(),
    );

    assert_eq!(provider, "lumvise-cloud");
    assert_eq!(text, SIGN_IN_MESSAGE);
    assert!(inner.requests.lock().unwrap().is_empty());
}

#[test]
fn unauthorized_renews_the_token_and_retries_once() {
    let inner = FakeInner::scripted(vec![
        reply(http_failure(
            "HTTP 401: {\"error\":{\"code\":\"unauthenticated\"}}",
        )),
        reply(Ok(json!({"text": "hi"}))),
    ]);
    let tokens = Arc::<FakeTokens>::default();

    let value = client(&inner, &tokens).post_json(&request()).unwrap();

    assert_eq!(value, json!({"text": "hi"}));
    assert_eq!(*tokens.log.lock().unwrap(), [false, true]);
    assert_eq!(inner.credentials(), ["cached", "renewed"]);
}

#[test]
fn a_second_unauthorized_answer_becomes_the_sign_in_message() {
    let inner = FakeInner::scripted(vec![
        reply(http_failure("HTTP 401: nope")),
        reply(http_failure("HTTP 401: nope")),
    ]);

    let (provider, text) = message(
        client(&inner, &Arc::default())
            .post_json(&request())
            .unwrap_err(),
    );

    assert_eq!(provider, "lumvise-cloud");
    assert_eq!(text, SIGN_IN_MESSAGE);
    assert_eq!(inner.requests.lock().unwrap().len(), 2);
}

#[test]
fn exhausted_balance_and_daily_limit_are_explained() {
    for (failure, expected) in [
        (
            "HTTP 402: {\"error\":{\"code\":\"token_balance_exhausted\"}}",
            BALANCE_MESSAGE,
        ),
        (
            "HTTP 429: {\"error\":{\"code\":\"daily_limit_reached\"}}",
            DAILY_LIMIT_MESSAGE,
        ),
    ] {
        let inner = FakeInner::scripted(vec![reply(http_failure(failure))]);
        let error = client(&inner, &Arc::default())
            .post_json(&request())
            .unwrap_err();
        assert_eq!(message(error), ("lumvise-cloud".into(), expected.into()));
    }
}

#[test]
fn other_failures_pass_through_unchanged() {
    for failure in [
        "HTTP 429: {\"error\":{\"code\":\"busy\"}}",
        "HTTP 502: {\"error\":{\"code\":\"upstream_failed\"}}",
    ] {
        let inner = FakeInner::scripted(vec![reply(http_failure(failure))]);
        let error = client(&inner, &Arc::default())
            .post_json(&request())
            .unwrap_err();
        let (provider, text) = message(error);
        assert_eq!(provider, request().endpoint);
        assert_eq!(text, failure);
    }
}

#[test]
fn streams_retry_after_unauthorized_only_before_the_first_chunk() {
    let before = FakeInner::scripted(vec![
        reply(http_failure("HTTP 401: nope")),
        Reply {
            chunks: vec!["a".into()],
            outcome: Ok(json!({})),
        },
    ]);
    let mut seen = Vec::new();
    client(&before, &Arc::default())
        .stream_text_until(&request(), &mut |chunk| {
            seen.push(chunk);
            Ok(ControlFlow::Continue(()))
        })
        .unwrap();
    assert_eq!(seen, ["a"]);
    assert_eq!(before.credentials(), ["cached", "renewed"]);

    let after = FakeInner::scripted(vec![Reply {
        chunks: vec!["a".into()],
        outcome: http_failure("HTTP 401: nope"),
    }]);
    let error = client(&after, &Arc::default())
        .stream_text_until(&request(), &mut |_| Ok(ControlFlow::Continue(())))
        .unwrap_err();
    assert_eq!(message(error).1, SIGN_IN_MESSAGE);
    assert_eq!(after.requests.lock().unwrap().len(), 1);
}

#[test]
fn speech_adapters_call_the_website_audio_endpoints_with_the_account_token() {
    let inner = FakeInner::scripted(vec![
        reply(Ok(json!({"text": "hello"}))),
        reply(Ok(json!({}))),
    ]);
    let client: Arc<dyn LlmHttpClient> = Arc::new(client(&inner, &Arc::default()));
    let config = OpenRouterSpeechConfig {
        endpoint: "https://app.lumvise.com/api/public/v1/ai".into(),
        credential: String::new(),
        model: "lumvise-cloud".into(),
        voice: None,
        timeout_ms: 60_000,
    };
    let recognizer = OpenRouterSpeechRecognizer::new(config.clone(), Arc::clone(&client));
    let synthesizer = OpenRouterSpeechSynthesizer::new(config, client);
    let control = InvocationControl::sixty_seconds();

    let transcript = recognizer
        .transcribe(
            &Voice2TextRequest {
                audio: b"audio".to_vec(),
                media_type: "audio/wav".into(),
                model: None,
            },
            &control,
        )
        .unwrap();
    synthesizer
        .synthesize(
            &Text2VoiceRequest {
                text: "hello".into(),
                voice_id: None,
                model: None,
            },
            &control,
        )
        .unwrap();

    assert_eq!(transcript.transcript, "hello");
    let requests = inner.requests.lock().unwrap();
    let endpoints: Vec<_> = requests
        .iter()
        .map(|request| request.endpoint.as_str())
        .collect();
    assert_eq!(
        endpoints,
        [
            "https://app.lumvise.com/api/public/v1/ai/audio/transcriptions",
            "https://app.lumvise.com/api/public/v1/ai/audio/speech",
        ]
    );
    assert!(
        requests
            .iter()
            .all(|request| request.credential == "cached")
    );
}
