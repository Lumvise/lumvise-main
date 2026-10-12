//! Serves an LLM HTTP client's calls under an account's bearer token, renewed on demand.
//! The caller never holds a provider key; the token is attached per call.

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
    fn bearer_token(&self, force_refresh: bool) -> std::result::Result<String, String>;
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
        delivered: &Cell<bool>,
        mut call: impl FnMut(&LlmHttpRequest) -> Result<T>,
    ) -> Result<T> {
        let mut force_refresh = false;
        loop {
            let credential = self
                .token
                .bearer_token(force_refresh)
                .map_err(|_| cloud_error(SIGN_IN_MESSAGE))?;
            let request = LlmHttpRequest {
                credential,
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
        self.send(request, &Cell::new(false), |request| {
            self.inner.post_json(request)
        })
    }

    fn post_bytes(&self, request: &LlmHttpRequest) -> Result<Vec<u8>> {
        self.send(request, &Cell::new(false), |request| {
            self.inner.post_bytes(request)
        })
    }

    fn get_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        self.send(request, &Cell::new(false), |request| {
            self.inner.get_json(request)
        })
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> Result<Vec<String>> {
        self.send(request, &Cell::new(false), |request| {
            self.inner.stream_text(request)
        })
    }

    fn stream_text_until(
        &self,
        request: &LlmHttpRequest,
        on_chunk: &mut dyn FnMut(String) -> Result<ControlFlow<()>>,
    ) -> Result<()> {
        let delivered = Cell::new(false);
        self.send(request, &delivered, |request| {
            self.inner.stream_text_until(request, &mut |chunk| {
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
        let delivered = Cell::new(false);
        self.send(request, &delivered, |request| {
            self.inner.stream_text_with_chunks(request, &mut |chunk| {
                delivered.set(true);
                on_chunk(chunk)
            })
        })
    }
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
