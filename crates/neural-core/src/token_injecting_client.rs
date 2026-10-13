//! Serves an LLM HTTP client's calls under an account's bearer token, renewed on demand.
//! The caller never holds a provider key; the token is attached per call.

use lumvise_resource_routing::InvocationControl;
use std::cell::Cell;
use std::ops::ControlFlow;
use std::sync::Arc;

use serde_json::Value;

use crate::error::{NeuralError, Result};
use crate::llm_providers::contract::{LlmHttpClient, LlmHttpRequest, LlmTextChunkSink};

const PROVIDER_ID: &str = "lumvise-cloud";
const SIGN_IN_MESSAGE: &str = "Sign in to your Lumvise account.";
const BALANCE_MESSAGE: &str = "Lumvise token balance is used up.";
const DAILY_LIMIT_MESSAGE: &str = "Daily Lumvise cloud limit reached.";

/// Returns a currently valid bearer token; `force_refresh` renews it, e.g. `source.bearer_token(false)`.
pub trait BearerTokenSource: Send + Sync {
    fn bearer_token(
        &self,
        force_refresh: bool,
        control: &InvocationControl,
    ) -> std::result::Result<String, String>;
}

/// Puts a fresh account token on every call and turns website refusals into actionable messages.
pub struct TokenInjectingClient {
    inner: Arc<dyn LlmHttpClient>,
    token: Arc<dyn BearerTokenSource>,
}

impl TokenInjectingClient {
    /// Wraps `inner` so every call carries `token`'s bearer, e.g. `TokenInjectingClient::new(http, source)`.
    pub fn new(inner: Arc<dyn LlmHttpClient>, token: Arc<dyn BearerTokenSource>) -> Self {
        Self { inner, token }
    }

    /// Sends with a fresh token. A 401 renews the token and retries once, unless
    /// `delivered` shows the caller already received streamed data.
    fn send<T>(
        &self,
        request: &LlmHttpRequest,
        control: &InvocationControl,
        delivered: &Cell<bool>,
        mut call: impl FnMut(&LlmHttpRequest) -> Result<T>,
    ) -> Result<T> {
        let mut force_refresh = false;
        loop {
            crate::speech::ensure_active(control, PROVIDER_ID)?;
            let credential = self.token.bearer_token(force_refresh, control);
            crate::speech::ensure_active(control, PROVIDER_ID)?;
            let credential = credential.map_err(|message| cloud_error(&message))?;
            let request = LlmHttpRequest {
                credential,
                timeout: Some(
                    request
                        .timeout
                        .unwrap_or(control.remaining())
                        .min(control.remaining()),
                ),
                ..request.clone()
            };
            match call(&request) {
                Err(error) if !force_refresh && !delivered.get() && has_status(&error, "401") => {
                    force_refresh = true;
                }
                result => return result.map_err(cloud_failure),
            }
        }
    }
}

impl LlmHttpClient for TokenInjectingClient {
    fn post_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        self.post_json_controlled(request, &request_control(request))
    }

    fn post_bytes(&self, request: &LlmHttpRequest) -> Result<Vec<u8>> {
        self.post_bytes_controlled(request, &request_control(request))
    }

    fn post_json_controlled(
        &self,
        request: &LlmHttpRequest,
        control: &InvocationControl,
    ) -> Result<Value> {
        self.send(request, control, &Cell::new(false), |request| {
            self.inner.post_json_controlled(request, control)
        })
    }

    fn post_bytes_controlled(
        &self,
        request: &LlmHttpRequest,
        control: &InvocationControl,
    ) -> Result<Vec<u8>> {
        self.send(request, control, &Cell::new(false), |request| {
            self.inner.post_bytes_controlled(request, control)
        })
    }

    fn stream_bytes_controlled(
        &self,
        request: &LlmHttpRequest,
        control: &InvocationControl,
        on_chunk: &mut dyn FnMut(Vec<u8>) -> Result<()>,
    ) -> Result<()> {
        let delivered = Cell::new(false);
        self.send(request, control, &delivered, |request| {
            self.inner
                .stream_bytes_controlled(request, control, &mut |chunk| {
                    delivered.set(true);
                    on_chunk(chunk)
                })
        })
    }

    fn get_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        self.send(
            request,
            &request_control(request),
            &Cell::new(false),
            |request| self.inner.get_json(request),
        )
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> Result<Vec<String>> {
        let mut chunks = Vec::new();
        self.stream_text_with_chunks(request, &mut |chunk| {
            chunks.push(chunk);
            Ok(())
        })?;
        Ok(chunks)
    }

    fn stream_text_until(
        &self,
        request: &LlmHttpRequest,
        on_chunk: &mut dyn FnMut(String) -> Result<ControlFlow<()>>,
    ) -> Result<()> {
        self.stream_text_until_controlled(request, &request_control(request), on_chunk)
    }

    fn stream_text_until_controlled(
        &self,
        request: &LlmHttpRequest,
        control: &InvocationControl,
        on_chunk: &mut dyn FnMut(String) -> Result<ControlFlow<()>>,
    ) -> Result<()> {
        let delivered = Cell::new(false);
        self.send(request, control, &delivered, |request| {
            self.inner
                .stream_text_until_controlled(request, control, &mut |chunk| {
                    delivered.set(true);
                    on_chunk(chunk)
                })
        })
    }

    fn stream_text_with_chunks(
        &self,
        request: &LlmHttpRequest,
        on_chunk: &mut LlmTextChunkSink<'_>,
    ) -> Result<()> {
        self.stream_text_until(request, &mut |chunk| {
            on_chunk(chunk)?;
            Ok(ControlFlow::Continue(()))
        })
    }
}

fn request_control(request: &LlmHttpRequest) -> InvocationControl {
    InvocationControl::with_deadline(
        request
            .timeout
            .unwrap_or(std::time::Duration::from_secs(60)),
    )
}

fn cloud_error(message: &str) -> NeuralError {
    NeuralError::ProviderFailed {
        provider_id: PROVIDER_ID.into(),
        message: message.into(),
    }
}

fn has_status(error: &NeuralError, status: &str) -> bool {
    matches!(error, NeuralError::ProviderFailed { message, .. }
        if message.strip_prefix("HTTP ").is_some_and(|rest| rest.starts_with(status)))
}

/// Replaces the website's refusals with messages the user can act on; other errors pass through.
fn cloud_failure(error: NeuralError) -> NeuralError {
    // shortcut: the HTTP status is only available as the "HTTP <status>" message prefix, give NeuralError a typed status when another caller needs it.
    if has_status(&error, "401") {
        cloud_error(SIGN_IN_MESSAGE)
    } else if has_status(&error, "402") {
        cloud_error(BALANCE_MESSAGE)
    } else if has_status(&error, "429")
        && matches!(&error, NeuralError::ProviderFailed { message, .. }
            if message.contains("daily_limit_reached"))
    {
        cloud_error(DAILY_LIMIT_MESSAGE)
    } else {
        error
    }
}
