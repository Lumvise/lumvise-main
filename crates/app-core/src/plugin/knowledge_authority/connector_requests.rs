//! Assistant-to-connector request ledger. The Assistant appends requests under its
//! live session; only the addressed connector can list or resolve them. Requests
//! are data for the connector to decide on, never commands the host executes.
use super::{ASSISTANT, KnowledgeAuthority, invalid, storage};
use crate::plugin::host_capability_catalog::CONNECTOR_REQUEST_CAPABILITY;
use lumvise_contracts::ConnectorIdentity;
use lumvise_plugin_runtime::{HostCapabilityError, PluginInvocationContext};
use lumvise_resource_routing::InvocationControl;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_REQUEST_CHARS: usize = 4_000;
const MAX_TITLE_CHARS: usize = 200;
const MAX_NOTE_CHARS: usize = 1_000;
const ASSISTANT_REQUEST_ORIGIN: &str = "assistant_request";

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum ConnectorRequestOperation {
    Connectors {},
    Append {
        connector_id: String,
        #[serde(default)]
        title: Option<String>,
        request: String,
    },
    List {
        #[serde(default)]
        status: Option<ListScope>,
    },
    Resolve {
        request_id: String,
        expected_revision: u64,
        status: RequestStatus,
        #[serde(default)]
        note: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ListScope {
    Open,
    All,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RequestStatus {
    Open,
    Done,
    Declined,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ConnectorRequestRecord {
    request_id: String,
    connector_id: String,
    connector_display_name: String,
    origin: String,
    assistant_session_id: String,
    title: String,
    request: String,
    created_at: u64,
    status: RequestStatus,
    revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolved_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolution_note: Option<String>,
}

impl KnowledgeAuthority {
    pub(in crate::plugin) fn connector_request(
        &self,
        plugin: &str,
        input: Value,
        context: &PluginInvocationContext,
    ) -> Result<Value, HostCapabilityError> {
        let bridge = crate::plugin::plugin_host_capabilities::invocation_control(context);
        let control = bridge.control();
        let operation: ConnectorRequestOperation =
            serde_json::from_value(input.clone()).map_err(|_| {
                request_error(
                    "connector_request_invalid",
                    format!(
                        "invalid input `{input}`; expected connectors, append, list, or resolve"
                    ),
                )
            })?;
        if plugin != ASSISTANT {
            return Err(denied(plugin, "builtin.assistant invocation"));
        }
        storage::ensure(self.relational.as_ref(), &control)?;
        match operation {
            ConnectorRequestOperation::Connectors {} => {
                self.live_assistant_session(context, &control)?;
                self.list_connectors(&control)
            }
            ConnectorRequestOperation::Append {
                connector_id,
                title,
                request,
            } => {
                let session = self.live_assistant_session(context, &control)?;
                self.append_request(session, &connector_id, title, &request, &control)
            }
            ConnectorRequestOperation::List { status } => {
                let connector_id = self.origin_connector(context)?;
                let scope = status.unwrap_or(ListScope::Open);
                self.list_requests(&connector_id, scope, &control)
            }
            ConnectorRequestOperation::Resolve {
                request_id,
                expected_revision,
                status,
                note,
            } => {
                let connector_id = self.origin_connector(context)?;
                self.resolve_request(
                    &connector_id,
                    &request_id,
                    expected_revision,
                    status,
                    note,
                    &control,
                )
            }
        }
    }

    /// Returns the bound live Assistant session that owns this invocation.
    fn live_assistant_session<'a>(
        &self,
        context: &'a PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<&'a str, HostCapabilityError> {
        self.live_assistant_binding(context, control)?;
        context
            .session_id()
            .ok_or_else(|| denied("assistant session", "bound live Assistant session"))
    }

    /// Returns the connector that the transport proved; list and resolve never take it from input.
    fn origin_connector(
        &self,
        context: &PluginInvocationContext,
    ) -> Result<String, HostCapabilityError> {
        let origin = context
            .mcp_origin()
            .ok_or_else(|| denied("no MCP origin", "identified connector MCP origin"))?;
        let connector = origin
            .connector_id
            .as_deref()
            .ok_or_else(|| denied("connector_id None", "identified connector origin"))?;
        self.mcp_membership
            .require_connector(&origin.connection_id, Some(connector))
            .map_err(|error| denied(&error, "matching authenticated connector proof"))?;
        Ok(connector.to_owned())
    }

    fn list_connectors(&self, control: &InvocationControl) -> Result<Value, HostCapabilityError> {
        let connectors = storage::list_prefix(self.relational.as_ref(), "connector:", control)?
            .into_iter()
            .map(|row| {
                serde_json::from_value::<ConnectorIdentity>(row.clone())
                    .map(|identity| {
                        json!({"connector_id": identity.connector_id, "display_name": identity.display_name})
                    })
                    .map_err(|_| invalid(&row, "registered connector identity"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({"connectors": connectors}))
    }

    fn append_request(
        &self,
        session: &str,
        connector_id: &str,
        title: Option<String>,
        request: &str,
        control: &InvocationControl,
    ) -> Result<Value, HostCapabilityError> {
        let request = request.trim();
        if request.is_empty() || request.chars().count() > MAX_REQUEST_CHARS {
            return Err(request_error(
                "connector_request_invalid",
                format!("request must be 1 to {MAX_REQUEST_CHARS} characters"),
            ));
        }
        let title = match title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
        {
            Some(title) if title.chars().count() > MAX_TITLE_CHARS => {
                return Err(request_error(
                    "connector_request_invalid",
                    format!("title must be at most {MAX_TITLE_CHARS} characters"),
                ));
            }
            Some(title) => title.to_owned(),
            None => request
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(MAX_TITLE_CHARS)
                .collect(),
        };
        let connector = storage::read(
            self.relational.as_ref(),
            &format!("connector:{connector_id}"),
            control,
        )?
        .ok_or_else(|| request_error("connector_request_invalid", "registered connector"))
        .and_then(|row| {
            serde_json::from_value::<ConnectorIdentity>(row.clone())
                .map_err(|_| invalid(&row, "registered connector identity"))
        })?;
        let record = ConnectorRequestRecord {
            request_id: uuid::Uuid::now_v7().to_string(),
            connector_id: connector.connector_id,
            connector_display_name: connector.display_name,
            origin: ASSISTANT_REQUEST_ORIGIN.into(),
            assistant_session_id: session.into(),
            title,
            request: request.into(),
            created_at: now_ms(),
            status: RequestStatus::Open,
            revision: 1,
            resolved_at: None,
            resolution_note: None,
        };
        let key = record_key(&record.connector_id, &record.request_id);
        storage::replace(
            self.relational.as_ref(),
            &key,
            None,
            json!(&record),
            control,
        )?;
        Ok(json!({"request": record}))
    }

    fn list_requests(
        &self,
        connector_id: &str,
        scope: ListScope,
        control: &InvocationControl,
    ) -> Result<Value, HostCapabilityError> {
        let mut requests = storage::list_prefix(
            self.relational.as_ref(),
            &connector_requests_prefix(connector_id),
            control,
        )?
        .into_iter()
        .map(decode_record)
        .collect::<Result<Vec<_>, _>>()?;
        requests.retain(|request| scope == ListScope::All || request.status == RequestStatus::Open);
        requests.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.request_id.cmp(&left.request_id))
        });
        Ok(json!({"requests": requests}))
    }

    fn resolve_request(
        &self,
        connector_id: &str,
        request_id: &str,
        expected_revision: u64,
        status: RequestStatus,
        note: Option<String>,
        control: &InvocationControl,
    ) -> Result<Value, HostCapabilityError> {
        if status == RequestStatus::Open {
            return Err(request_error(
                "connector_request_invalid",
                "status must be done or declined",
            ));
        }
        let note = note
            .map(|note| note.trim().to_owned())
            .filter(|note| !note.is_empty());
        if note
            .as_ref()
            .is_some_and(|note| note.chars().count() > MAX_NOTE_CHARS)
        {
            return Err(request_error(
                "connector_request_invalid",
                format!("note must be at most {MAX_NOTE_CHARS} characters"),
            ));
        }
        // The key carries the proven connector, so another connector's request reads as absent.
        let key = record_key(connector_id, request_id);
        let previous =
            storage::read(self.relational.as_ref(), &key, control)?.ok_or_else(|| {
                request_error("connector_request_not_found", "request for this connector")
            })?;
        let current = decode_record(previous.clone())?;
        if current.status != RequestStatus::Open {
            return Err(request_error(
                "connector_request_conflict",
                format!("request already {:?}", current.status).to_lowercase(),
            ));
        }
        if current.revision != expected_revision {
            return Err(request_error(
                "connector_request_conflict",
                format!(
                    "expected revision {expected_revision}; current revision {}",
                    current.revision
                ),
            ));
        }
        let revision = current.revision + 1;
        let resolved = ConnectorRequestRecord {
            status,
            revision,
            resolved_at: Some(now_ms()),
            resolution_note: note,
            ..current
        };
        storage::replace(
            self.relational.as_ref(),
            &key,
            Some(previous),
            json!(&resolved),
            control,
        )?;
        Ok(json!({"request": resolved}))
    }
}

fn connector_requests_prefix(connector_id: &str) -> String {
    format!("connector-request:{connector_id}:")
}

fn record_key(connector_id: &str, request_id: &str) -> String {
    format!("{}{request_id}", connector_requests_prefix(connector_id))
}

fn decode_record(row: Value) -> Result<ConnectorRequestRecord, HostCapabilityError> {
    serde_json::from_value(row.clone()).map_err(|_| invalid(&row, "stored connector request"))
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

fn denied(value: &str, expected: &str) -> HostCapabilityError {
    request_error(
        "connector_request_denied",
        format!("connector request authority `{value}`; expected {expected}"),
    )
}

fn request_error(code: &str, message: impl Into<String>) -> HostCapabilityError {
    HostCapabilityError::new(CONNECTOR_REQUEST_CAPABILITY, code, message.into(), false)
}
