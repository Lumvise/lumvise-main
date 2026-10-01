use std::str::FromStr;

use prost::Message;
use serde_json::{Map, Number, Value};

use crate::{
    MessageBody, PluginWireError, ProtocolError, ProtocolVersion, WireMessage, WireOutcome,
};

#[derive(Clone, PartialEq, Message)]
pub(crate) struct Envelope {
    #[prost(uint32, tag = "1")]
    protocol_major: u32,
    #[prost(uint32, tag = "2")]
    protocol_minor: u32,
    #[prost(oneof = "envelope::Body", tags = "10, 11, 12, 13, 14, 15, 16, 17, 18")]
    body: Option<envelope::Body>,
}

mod envelope {
    use super::{Completion, Handshake, HostCall, Invocation, SessionControl};
    use prost::Oneof;

    #[derive(Clone, PartialEq, Oneof)]
    pub(super) enum Body {
        #[prost(message, tag = "10")]
        HostHello(Handshake),
        #[prost(message, tag = "11")]
        PluginReady(Handshake),
        #[prost(message, tag = "12")]
        HostInvoke(Invocation),
        #[prost(message, tag = "13")]
        PluginResult(Completion),
        #[prost(message, tag = "14")]
        PluginHostCall(HostCall),
        #[prost(message, tag = "15")]
        HostHostResult(Completion),
        #[prost(message, tag = "16")]
        HostCancel(SessionControl),
        #[prost(message, tag = "17")]
        HostShutdown(SessionControl),
        #[prost(message, tag = "18")]
        PluginStopped(SessionControl),
    }
}

#[derive(Clone, PartialEq, Message)]
struct Handshake {
    #[prost(string, tag = "1")]
    session_id: String,
    #[prost(string, tag = "2")]
    identity: String,
    #[prost(string, tag = "3")]
    package_digest: String,
}

#[derive(Clone, PartialEq, Message)]
struct Invocation {
    #[prost(string, tag = "1")]
    session_id: String,
    #[prost(string, tag = "2")]
    invocation_id: String,
    #[prost(string, tag = "3")]
    capability_id: String,
    #[prost(message, optional, tag = "4")]
    input: Option<ProtoValue>,
}

#[derive(Clone, PartialEq, Message)]
struct Completion {
    #[prost(string, tag = "1")]
    session_id: String,
    #[prost(string, tag = "2")]
    correlation_id: String,
    #[prost(message, optional, tag = "3")]
    outcome: Option<ProtoOutcome>,
}

#[derive(Clone, PartialEq, Message)]
struct HostCall {
    #[prost(string, tag = "1")]
    session_id: String,
    #[prost(string, tag = "2")]
    invocation_id: String,
    #[prost(string, tag = "3")]
    call_id: String,
    #[prost(string, tag = "4")]
    capability_id: String,
    #[prost(message, optional, tag = "5")]
    input: Option<ProtoValue>,
}

#[derive(Clone, PartialEq, Message)]
struct SessionControl {
    #[prost(string, tag = "1")]
    session_id: String,
    #[prost(string, optional, tag = "2")]
    value: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
struct ProtoOutcome {
    #[prost(oneof = "proto_outcome::Result", tags = "1, 2")]
    result: Option<proto_outcome::Result>,
}

mod proto_outcome {
    use super::{ProtoPluginError, ProtoValue};
    use prost::Oneof;

    #[derive(Clone, PartialEq, Oneof)]
    pub(super) enum Result {
        #[prost(message, tag = "1")]
        Succeeded(ProtoValue),
        #[prost(message, tag = "2")]
        Failed(ProtoPluginError),
    }
}

#[derive(Clone, PartialEq, Message)]
struct ProtoPluginError {
    #[prost(string, tag = "1")]
    code: String,
    #[prost(string, tag = "2")]
    message: String,
    #[prost(message, optional, tag = "3")]
    details: Option<ProtoValue>,
    #[prost(bool, tag = "4")]
    retryable: bool,
}

#[derive(Clone, PartialEq, Message)]
struct ProtoValue {
    #[prost(oneof = "proto_value::Kind", tags = "1, 2, 3, 4, 5, 6")]
    kind: Option<proto_value::Kind>,
}

mod proto_value {
    use super::{ProtoList, ProtoObject};
    use prost::Oneof;

    #[derive(Clone, PartialEq, Oneof)]
    pub(super) enum Kind {
        #[prost(bool, tag = "1")]
        Null(bool),
        #[prost(bool, tag = "2")]
        Bool(bool),
        #[prost(string, tag = "3")]
        Number(String),
        #[prost(string, tag = "4")]
        String(String),
        #[prost(message, tag = "5")]
        List(ProtoList),
        #[prost(message, tag = "6")]
        Object(ProtoObject),
    }
}

#[derive(Clone, PartialEq, Message)]
struct ProtoList {
    #[prost(message, repeated, tag = "1")]
    values: Vec<ProtoValue>,
}

#[derive(Clone, PartialEq, Message)]
struct ProtoObject {
    #[prost(message, repeated, tag = "1")]
    fields: Vec<ProtoField>,
}

#[derive(Clone, PartialEq, Message)]
struct ProtoField {
    #[prost(string, tag = "1")]
    key: String,
    #[prost(message, optional, tag = "2")]
    value: Option<ProtoValue>,
}

impl From<&WireMessage> for Envelope {
    fn from(message: &WireMessage) -> Self {
        Self {
            protocol_major: message.protocol.major.into(),
            protocol_minor: message.protocol.minor.into(),
            body: Some((&message.body).into()),
        }
    }
}

impl TryFrom<Envelope> for WireMessage {
    type Error = ProtocolError;

    fn try_from(envelope: Envelope) -> Result<Self, Self::Error> {
        let protocol = ProtocolVersion::new(
            narrow_version("major", envelope.protocol_major)?,
            narrow_version("minor", envelope.protocol_minor)?,
        );
        let body = envelope
            .body
            .ok_or_else(|| invalid_message("body is absent", "one protocol operation"))?
            .try_into()?;
        Ok(Self { protocol, body })
    }
}

impl From<&MessageBody> for envelope::Body {
    fn from(body: &MessageBody) -> Self {
        match body {
            MessageBody::HostHello {
                session_id,
                host_id,
                package_digest,
            } => Self::HostHello(handshake(session_id, host_id, package_digest)),
            MessageBody::PluginReady {
                session_id,
                plugin_id,
                package_digest,
            } => Self::PluginReady(handshake(session_id, plugin_id, package_digest)),
            MessageBody::HostInvoke {
                session_id,
                invocation_id,
                capability_id,
                input,
            } => Self::HostInvoke(invocation(session_id, invocation_id, capability_id, input)),
            MessageBody::PluginResult {
                session_id,
                invocation_id,
                outcome,
            } => Self::PluginResult(completion(session_id, invocation_id, outcome)),
            MessageBody::PluginHostCall {
                session_id,
                invocation_id,
                call_id,
                capability_id,
                input,
            } => Self::PluginHostCall(HostCall {
                session_id: session_id.clone(),
                invocation_id: invocation_id.clone(),
                call_id: call_id.clone(),
                capability_id: capability_id.clone(),
                input: Some(input.into()),
            }),
            MessageBody::HostHostResult {
                session_id,
                call_id,
                outcome,
            } => Self::HostHostResult(completion(session_id, call_id, outcome)),
            MessageBody::HostCancel {
                session_id,
                invocation_id,
            } => Self::HostCancel(control(session_id, Some(invocation_id))),
            MessageBody::HostShutdown { session_id, reason } => {
                Self::HostShutdown(control(session_id, reason.as_deref()))
            }
            MessageBody::PluginStopped { session_id, reason } => {
                Self::PluginStopped(control(session_id, reason.as_deref()))
            }
        }
    }
}

impl TryFrom<envelope::Body> for MessageBody {
    type Error = ProtocolError;

    fn try_from(body: envelope::Body) -> Result<Self, Self::Error> {
        Ok(match body {
            envelope::Body::HostHello(value) => Self::HostHello {
                session_id: value.session_id,
                host_id: value.identity,
                package_digest: value.package_digest,
            },
            envelope::Body::PluginReady(value) => Self::PluginReady {
                session_id: value.session_id,
                plugin_id: value.identity,
                package_digest: value.package_digest,
            },
            envelope::Body::HostInvoke(value) => Self::HostInvoke {
                session_id: value.session_id,
                invocation_id: value.invocation_id,
                capability_id: value.capability_id,
                input: required_value(value.input, "host.invoke.input")?,
            },
            envelope::Body::PluginResult(value) => Self::PluginResult {
                session_id: value.session_id,
                invocation_id: value.correlation_id,
                outcome: required_outcome(value.outcome, "plugin.result.outcome")?,
            },
            envelope::Body::PluginHostCall(value) => Self::PluginHostCall {
                session_id: value.session_id,
                invocation_id: value.invocation_id,
                call_id: value.call_id,
                capability_id: value.capability_id,
                input: required_value(value.input, "plugin.host_call.input")?,
            },
            envelope::Body::HostHostResult(value) => Self::HostHostResult {
                session_id: value.session_id,
                call_id: value.correlation_id,
                outcome: required_outcome(value.outcome, "host.host_result.outcome")?,
            },
            envelope::Body::HostCancel(value) => Self::HostCancel {
                session_id: value.session_id,
                invocation_id: required_text(value.value, "host.cancel.invocation_id")?,
            },
            envelope::Body::HostShutdown(value) => Self::HostShutdown {
                session_id: value.session_id,
                reason: value.value,
            },
            envelope::Body::PluginStopped(value) => Self::PluginStopped {
                session_id: value.session_id,
                reason: value.value,
            },
        })
    }
}

impl From<&Value> for ProtoValue {
    fn from(value: &Value) -> Self {
        use proto_value::Kind;
        let kind = match value {
            Value::Null => Kind::Null(true),
            Value::Bool(value) => Kind::Bool(*value),
            Value::Number(value) => Kind::Number(value.to_string()),
            Value::String(value) => Kind::String(value.clone()),
            Value::Array(values) => Kind::List(ProtoList {
                values: values.iter().map(Into::into).collect(),
            }),
            Value::Object(fields) => Kind::Object(ProtoObject {
                fields: fields
                    .iter()
                    .map(|(key, value)| ProtoField {
                        key: key.clone(),
                        value: Some(value.into()),
                    })
                    .collect(),
            }),
        };
        Self { kind: Some(kind) }
    }
}

impl TryFrom<ProtoValue> for Value {
    type Error = ProtocolError;

    fn try_from(value: ProtoValue) -> Result<Self, Self::Error> {
        use proto_value::Kind;
        Ok(
            match value
                .kind
                .ok_or_else(|| invalid_message("value kind is absent", "one value kind"))?
            {
                Kind::Null(_) => Self::Null,
                Kind::Bool(value) => Self::Bool(value),
                Kind::Number(value) => Self::Number(Number::from_str(&value).map_err(|error| {
                    invalid_message(&format!("number `{value}`: {error}"), "a JSON number")
                })?),
                Kind::String(value) => Self::String(value),
                Kind::List(value) => Self::Array(
                    value
                        .values
                        .into_iter()
                        .map(TryInto::try_into)
                        .collect::<Result<_, _>>()?,
                ),
                Kind::Object(value) => Self::Object(object_fields(value.fields)?),
            },
        )
    }
}

fn handshake(session_id: &str, identity: &str, package_digest: &str) -> Handshake {
    Handshake {
        session_id: session_id.into(),
        identity: identity.into(),
        package_digest: package_digest.into(),
    }
}

fn invocation(
    session_id: &str,
    invocation_id: &str,
    capability_id: &str,
    input: &Value,
) -> Invocation {
    Invocation {
        session_id: session_id.into(),
        invocation_id: invocation_id.into(),
        capability_id: capability_id.into(),
        input: Some(input.into()),
    }
}

fn completion(session_id: &str, correlation_id: &str, outcome: &WireOutcome) -> Completion {
    Completion {
        session_id: session_id.into(),
        correlation_id: correlation_id.into(),
        outcome: Some(outcome.into()),
    }
}

fn control(session_id: &str, value: Option<&str>) -> SessionControl {
    SessionControl {
        session_id: session_id.into(),
        value: value.map(Into::into),
    }
}

impl From<&WireOutcome> for ProtoOutcome {
    fn from(outcome: &WireOutcome) -> Self {
        use proto_outcome::Result;
        let result = match outcome {
            WireOutcome::Succeeded { value } => Result::Succeeded(value.into()),
            WireOutcome::Failed { error } => Result::Failed(ProtoPluginError {
                code: error.code.clone(),
                message: error.message.clone(),
                details: error.details.as_ref().map(Into::into),
                retryable: error.retryable,
            }),
        };
        Self {
            result: Some(result),
        }
    }
}

impl TryFrom<ProtoOutcome> for WireOutcome {
    type Error = ProtocolError;

    fn try_from(outcome: ProtoOutcome) -> Result<Self, Self::Error> {
        use proto_outcome::Result;
        match outcome
            .result
            .ok_or_else(|| invalid_message("outcome is absent", "success or failure"))?
        {
            Result::Succeeded(value) => Ok(Self::Succeeded {
                value: value.try_into()?,
            }),
            Result::Failed(error) => Ok(Self::Failed {
                error: PluginWireError {
                    code: error.code,
                    message: error.message,
                    details: error.details.map(TryInto::try_into).transpose()?,
                    retryable: error.retryable,
                },
            }),
        }
    }
}

fn required_value(value: Option<ProtoValue>, field: &str) -> Result<Value, ProtocolError> {
    value
        .ok_or_else(|| invalid_message(&format!("{field} is absent"), "a Protobuf value"))?
        .try_into()
}

fn required_outcome(
    value: Option<ProtoOutcome>,
    field: &str,
) -> Result<WireOutcome, ProtocolError> {
    value
        .ok_or_else(|| invalid_message(&format!("{field} is absent"), "a Protobuf outcome"))?
        .try_into()
}

fn required_text(value: Option<String>, field: &str) -> Result<String, ProtocolError> {
    value.ok_or_else(|| invalid_message(&format!("{field} is absent"), "a string"))
}

fn narrow_version(field: &str, value: u32) -> Result<u16, ProtocolError> {
    u16::try_from(value).map_err(|_| {
        invalid_message(
            &format!("protocol {field} `{value}`"),
            "an unsigned 16-bit integer",
        )
    })
}

fn object_fields(fields: Vec<ProtoField>) -> Result<Map<String, Value>, ProtocolError> {
    fields
        .into_iter()
        .map(|field| {
            let value = required_value(field.value, &format!("object field `{}`", field.key))?;
            Ok((field.key, value))
        })
        .collect()
}

fn invalid_message(actual: &str, expected: &str) -> ProtocolError {
    ProtocolError::InvalidProtobufMessage {
        actual: actual.into(),
        expected: expected.into(),
    }
}
