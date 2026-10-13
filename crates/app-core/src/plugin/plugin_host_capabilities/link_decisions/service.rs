use std::sync::{Arc, OnceLock};

use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
use lumvise_neural_core::llm_providers::executor::LlmFailureCode;
use lumvise_neural_core::{NeuralError, llm_providers::http_client::ReqwestLlmHttpClient};
use lumvise_plugin_runtime::HostCapabilityError;
use lumvise_resource_routing::InvocationControl;

use super::{LinkDecisionOutcome, LinkDecisionReply, LinkDecisionRequest, LinkDecisionService};

#[path = "cloud.rs"]
mod cloud;
#[path = "jev.rs"]
mod jev;

const CAPABILITY: &str = "neural.decisions";

pub(super) fn decide(
    service: &LinkDecisionService,
    provider: lumvise_frontend_core::LinkClassificationProvider,
    request: LinkDecisionRequest,
    control: InvocationControl,
) -> Result<LinkDecisionReply, HostCapabilityError> {
    ensure_active(&control)?;
    match provider {
        lumvise_frontend_core::LinkClassificationProvider::Jev => {
            jev::decide(service, request, control)
        }
        lumvise_frontend_core::LinkClassificationProvider::LumviseCloud => {
            cloud::decide(service, request, control)
        }
    }
}

pub(super) fn production_http() -> Arc<dyn LlmHttpClient> {
    let client: Arc<dyn LlmHttpClient> = Arc::new(LazyReqwestLlmHttpClient::default());
    client
}

#[derive(Default)]
struct LazyReqwestLlmHttpClient(OnceLock<ReqwestLlmHttpClient>);

impl LlmHttpClient for LazyReqwestLlmHttpClient {
    fn post_json_controlled(
        &self,
        request: &LlmHttpRequest,
        control: &InvocationControl,
    ) -> lumvise_neural_core::Result<serde_json::Value> {
        self.0
            .get_or_init(ReqwestLlmHttpClient::new)
            .post_json_controlled(request, control)
    }
    fn post_json(
        &self,
        request: &LlmHttpRequest,
    ) -> lumvise_neural_core::Result<serde_json::Value> {
        self.0
            .get_or_init(ReqwestLlmHttpClient::new)
            .post_json(request)
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> lumvise_neural_core::Result<Vec<String>> {
        Err(NeuralError::ProviderFailed {
            provider_id: request.endpoint.clone(),
            message: "link-decision transport only supports JSON completion requests".into(),
        })
    }
}

fn ensure_active(control: &InvocationControl) -> Result<(), HostCapabilityError> {
    if control.is_cancelled() {
        return Err(HostCapabilityError::new(
            CAPABILITY,
            "cancelled",
            "link decision was cancelled before provider dispatch",
            false,
        ));
    }
    if control.is_expired() {
        return Err(HostCapabilityError::new(
            CAPABILITY,
            "deadline_exceeded",
            "link decision deadline elapsed before provider dispatch",
            true,
        ));
    }
    Ok(())
}

fn provider_failure(code: LlmFailureCode, retryable: bool) -> HostCapabilityError {
    let (code, message) = provider_failure_details(code);
    HostCapabilityError::new(CAPABILITY, code, message, retryable)
}

fn provider_failure_details(code: LlmFailureCode) -> (&'static str, &'static str) {
    let failure = ProviderFailureKind::from(code);
    (failure.code(), failure.message())
}

#[derive(Clone, Copy)]
enum ProviderFailureKind {
    Deadline,
    Cancelled,
    Unconfigured,
    AuthenticationRequired,
    Busy,
    MalformedResponse,
    Rejected,
    Transport,
    ProcessFailure,
    ToolRounds,
    Internal,
}

impl From<LlmFailureCode> for ProviderFailureKind {
    fn from(code: LlmFailureCode) -> Self {
        match code {
            LlmFailureCode::DeadlineExceeded => Self::Deadline,
            LlmFailureCode::Cancelled => Self::Cancelled,
            LlmFailureCode::ProviderUnconfigured => Self::Unconfigured,
            LlmFailureCode::AuthenticationRequired => Self::AuthenticationRequired,
            LlmFailureCode::ProviderBusy => Self::Busy,
            LlmFailureCode::MalformedProviderResponse => Self::MalformedResponse,
            LlmFailureCode::ProviderRejected => Self::Rejected,
            LlmFailureCode::TransportReset => Self::Transport,
            LlmFailureCode::ProcessCrashed => Self::ProcessFailure,
            LlmFailureCode::ToolRoundsExhausted => Self::ToolRounds,
            LlmFailureCode::AppInternal => Self::Internal,
        }
    }
}

impl ProviderFailureKind {
    fn code(self) -> &'static str {
        match self {
            Self::Deadline => "deadline_exceeded",
            Self::Cancelled => "cancelled",
            Self::Unconfigured => "provider_unconfigured",
            Self::AuthenticationRequired => "authentication_required",
            Self::Busy => "provider_busy",
            Self::MalformedResponse => "malformed_provider_response",
            Self::Rejected => "provider_rejected",
            Self::Transport => "transport_failed",
            Self::ProcessFailure | Self::ToolRounds => "provider_failed",
            Self::Internal => "internal_error",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::Deadline => "link decision provider exceeded the invocation deadline",
            Self::Cancelled => "link decision provider call was cancelled",
            Self::Unconfigured => "selected link decision provider is not configured",
            Self::AuthenticationRequired => {
                "sign in and configure the provider credentials before using link decisions"
            }
            Self::Busy => "selected link decision provider is busy; try again later",
            Self::MalformedResponse => {
                "selected link decision provider returned an invalid structured answer"
            }
            Self::Rejected => "selected link decision provider rejected the request",
            Self::Transport => "connection to the link decision provider failed",
            Self::ProcessFailure => "link decision provider worker failed",
            Self::ToolRounds => "link decision provider unexpectedly requested tool rounds",
            Self::Internal => "link decision provider could not be initialized",
        }
    }
}

fn malformed_response(provider: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        CAPABILITY,
        "malformed_provider_response",
        format!("{provider} returned no propagate/hold decision"),
        false,
    )
}

fn reply(
    decision: LinkDecisionOutcome,
    provider: lumvise_frontend_core::LinkClassificationProvider,
    model: String,
) -> LinkDecisionReply {
    LinkDecisionReply {
        decision,
        provider,
        model,
    }
}
