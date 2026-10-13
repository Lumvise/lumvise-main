//! Owns provider execution for dynamic Background Agent links. Host dispatch calls
//! `LinkDecisionService::decide`; provider adapters and response parsing stay private.

use std::sync::Arc;

use lumvise_frontend_core::LinkClassificationProvider;
use lumvise_neural_core::LlmProviderConfig;
use lumvise_neural_core::llm_providers::LlmExecutorRegistry;
use lumvise_neural_core::llm_providers::contract::LlmHttpClient;
use lumvise_plugin_runtime::HostCapabilityError;
use lumvise_resource_routing::InvocationControl;
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod service;
#[cfg(test)]
mod tests;

/// Native credentials never cross the plugin boundary; configured keys stay with their owner.
pub(crate) trait LinkDecisionCredentials: Send + Sync {
    fn openrouter_config(
        &self,
        control: &InvocationControl,
    ) -> Result<Option<LlmProviderConfig>, String>;
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LinkDecisionRequest {
    pub cause: String,
    pub baseline: Option<String>,
    pub current: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum LinkDecisionOutcome {
    Propagate,
    Hold,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct LinkDecisionReply {
    pub decision: LinkDecisionOutcome,
    pub provider: LinkClassificationProvider,
    pub model: String,
}

pub(super) struct LinkDecisionService {
    executors: Arc<LlmExecutorRegistry>,
    http: Arc<dyn LlmHttpClient>,
    credentials: Arc<dyn LinkDecisionCredentials>,
}

impl LinkDecisionService {
    pub(super) fn production(
        executors: Arc<LlmExecutorRegistry>,
        credentials: Arc<dyn LinkDecisionCredentials>,
    ) -> Self {
        Self::new(executors, service::production_http(), credentials)
    }

    pub(super) fn new(
        executors: Arc<LlmExecutorRegistry>,
        http: Arc<dyn LlmHttpClient>,
        credentials: Arc<dyn LinkDecisionCredentials>,
    ) -> Self {
        Self {
            executors,
            http,
            credentials,
        }
    }

    pub(super) fn decide(
        &self,
        provider: LinkClassificationProvider,
        input: Value,
        control: InvocationControl,
    ) -> Result<Value, HostCapabilityError> {
        let request = parse_request(input)?;
        let reply = service::decide(self, provider, request, control)?;
        serde_json::to_value(reply)
            .map_err(|error| super::failed("neural.decisions", &error.to_string()))
    }
}

fn parse_request(input: Value) -> Result<LinkDecisionRequest, HostCapabilityError> {
    let request: LinkDecisionRequest = serde_json::from_value(input).map_err(|error| {
        invalid_request(format!(
            "invalid link decision shape; expected cause, baseline and current: {error}"
        ))
    })?;
    if request.cause.trim().is_empty() {
        return Err(invalid_request(
            "cause is empty; expected a non-empty link condition".into(),
        ));
    }
    Ok(request)
}

fn invalid_request(message: String) -> HostCapabilityError {
    HostCapabilityError::new(
        "neural.decisions",
        "invalid_host_capability_input",
        message,
        false,
    )
}
