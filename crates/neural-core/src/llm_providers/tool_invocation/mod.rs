pub mod schema_adapters;

use crate::error::{NeuralError, Result};
use crate::llm_providers::LlmMcpServerConfig;
use jsonschema::Validator;
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// A model-visible MCP tool descriptor discovered from a scoped server.
#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// The classified result of one MCP tool invocation.
#[derive(Debug)]
pub enum ToolOutcome {
    Success(Value),
    ToolError(Value),
    Validation(String),
    Transport(NeuralError),
}

/// Immutable MCP descriptors and their private, trusted invocation routes.
pub struct McpToolCatalog {
    tools: Vec<McpTool>,
    invoker: McpToolInvoker,
}

/// Executes only tool names bound during catalog discovery.
pub struct McpToolInvoker {
    provider_id: String,
    routes: BTreeMap<String, McpToolRoute>,
    client: Client,
}

struct McpToolRoute {
    message_url: Option<String>,
    validator: std::result::Result<Validator, String>,
}

impl McpToolCatalog {
    pub fn discover(provider_id: &str, servers: &[LlmMcpServerConfig]) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|error| NeuralError::ProviderFailed {
                provider_id: provider_id.into(),
                message: format!("MCP HTTP client initialization failed: {error}"),
            })?;
        let mut routes = BTreeMap::new();
        let mut tools = Vec::new();
        for server in servers {
            discover_server_tools(provider_id, &client, server, &mut routes, &mut tools)?;
        }
        Ok(Self {
            tools,
            invoker: McpToolInvoker {
                provider_id: provider_id.to_string(),
                routes,
                client,
            },
        })
    }

    /// Creates a catalog whose tools are already bound by a trusted local
    /// transport. A caller must use [`McpToolInvoker::invoke_bound`] to execute
    /// the bound route; model-provided tool names are still validated here.
    pub fn from_bound_tools(provider_id: &str, tools: Vec<McpTool>) -> Result<Self> {
        let mut routes = BTreeMap::new();
        for tool in &tools {
            if tool.name.trim().is_empty() {
                return Err(NeuralError::ProviderFailed {
                    provider_id: provider_id.to_string(),
                    message: "bound MCP tool is missing a name".to_string(),
                });
            }
            if routes
                .insert(
                    tool.name.clone(),
                    McpToolRoute {
                        message_url: None,
                        validator: compile_validator(&tool.input_schema),
                    },
                )
                .is_some()
            {
                return Err(NeuralError::ProviderFailed {
                    provider_id: provider_id.to_string(),
                    message: format!("duplicate bound MCP tool `{}`", tool.name),
                });
            }
        }
        Ok(Self {
            tools,
            invoker: McpToolInvoker {
                provider_id: provider_id.to_string(),
                routes,
                client: Client::new(),
            },
        })
    }

    #[cfg(test)]
    pub(crate) fn empty(provider_id: &str) -> Self {
        Self {
            tools: Vec::new(),
            invoker: McpToolInvoker {
                provider_id: provider_id.to_string(),
                routes: BTreeMap::new(),
                client: Client::new(),
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    pub fn invoker(&self) -> &McpToolInvoker {
        &self.invoker
    }
}

impl McpToolInvoker {
    pub fn invoke(&self, name: &str, arguments: Value) -> ToolOutcome {
        let started = std::time::Instant::now();
        let outcome = match self.routes.get(name) {
            None => ToolOutcome::Validation(format!("unknown MCP tool `{name}`")),
            Some(route) => {
                if let Err(message) = validate_arguments(name, &route.validator, &arguments) {
                    ToolOutcome::Validation(message)
                } else {
                    let Some(message_url) = route.message_url.as_deref() else {
                        return record_tool_outcome(
                            ToolOutcome::Transport(NeuralError::ProviderFailed {
                                provider_id: self.provider_id.clone(),
                                message: "bound MCP tools require local invocation".to_string(),
                            }),
                            &self.provider_id,
                        );
                    };
                    match post_mcp_json(
                        &self.client,
                        &self.provider_id,
                        message_url,
                        json!({
                            "jsonrpc": "2.0",
                            "id": 2,
                            "method": "tools/call",
                            "params": { "name": name, "arguments": arguments }
                        }),
                    ) {
                        Ok(response) => classify_tool_call_response(&self.provider_id, response),
                        Err(error) => ToolOutcome::Transport(error),
                    }
                }
            }
        };
        tracing::debug!(provider_id = %self.provider_id, tool = name,
            elapsed_ms = started.elapsed().as_millis(),
            transport_failed = matches!(&outcome, ToolOutcome::Transport(_)),
            "MCP tool invocation completed");
        record_tool_outcome(outcome, &self.provider_id)
    }
    /// Invokes a locally bound route after validating that `name` and
    /// `arguments` belong to this catalog. The callback receives arguments
    /// only, never a model-controlled transport route.
    pub fn invoke_bound<E>(
        &self,
        name: &str,
        arguments: Value,
        execute: impl FnOnce(Value) -> std::result::Result<Value, E>,
    ) -> ToolOutcome
    where
        E: ToString,
    {
        let outcome = match self.routes.get(name) {
            None => ToolOutcome::Validation(format!("unknown MCP tool `{name}`")),
            Some(route) => {
                if let Err(message) = validate_arguments(name, &route.validator, &arguments) {
                    ToolOutcome::Validation(message)
                } else if route.message_url.is_some() {
                    ToolOutcome::Transport(NeuralError::ProviderFailed {
                        provider_id: self.provider_id.clone(),
                        message: "HTTP MCP tools require remote invocation".to_string(),
                    })
                } else {
                    match execute(arguments) {
                        Ok(value) => ToolOutcome::Success(value),
                        Err(error) => {
                            ToolOutcome::ToolError(json!({ "message": error.to_string() }))
                        }
                    }
                }
            }
        };
        record_tool_outcome(outcome, &self.provider_id)
    }
}

fn record_tool_outcome(outcome: ToolOutcome, engine: &str) -> ToolOutcome {
    let outcome_label = match &outcome {
        ToolOutcome::Success(_) => "success",
        ToolOutcome::ToolError(_) => "tool_error",
        ToolOutcome::Validation(_) => "malformed",
        ToolOutcome::Transport(_) => "transport_error",
    };
    metrics::counter!(
        "lumvise_assistant_tool_calls_total",
        "engine" => engine.to_owned(),
        "transport" => "mcp",
        "outcome" => outcome_label
    )
    .increment(1);
    outcome
}

/// Converts a dispatched [`ToolOutcome`] into the JSON payload a caller
/// feeds back to the model as the tool's result, or an error for the
/// transport failures a caller cannot recover from itself. Every LLM
/// provider's tool loop shares this one conversion.
pub(crate) fn tool_outcome_value(outcome: ToolOutcome) -> Result<Value> {
    match outcome {
        ToolOutcome::Success(result) | ToolOutcome::ToolError(result) => Ok(result),
        ToolOutcome::Validation(message) => Ok(json!({ "error": { "message": message } })),
        ToolOutcome::Transport(error) => Err(error),
    }
}

fn discover_server_tools(
    provider_id: &str,
    client: &Client,
    server: &LlmMcpServerConfig,
    routes: &mut BTreeMap<String, McpToolRoute>,
    tools: &mut Vec<McpTool>,
) -> Result<()> {
    let message_url = mcp_message_url(&server.url);
    for tool in mcp_tools_for_server(provider_id, client, &message_url)? {
        let name = required_tool_name(provider_id, &tool)?;
        let input_schema = input_schema(&tool);
        routes.insert(
            name.clone(),
            McpToolRoute {
                message_url: Some(message_url.clone()),
                validator: compile_validator(&input_schema),
            },
        );
        tools.push(mcp_tool(&tool, name, input_schema));
    }
    Ok(())
}

fn mcp_tools_for_server(
    provider_id: &str,
    client: &Client,
    message_url: &str,
) -> Result<Vec<Value>> {
    let response = post_mcp_json(
        client,
        provider_id,
        message_url,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    )?;
    Ok(response["result"]["tools"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

fn required_tool_name(provider_id: &str, tool: &Value) -> Result<String> {
    tool["name"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| NeuralError::ProviderFailed {
            provider_id: provider_id.to_string(),
            message: format!("MCP tool missing name: {tool}"),
        })
}

fn mcp_tool(tool: &Value, name: String, input_schema: Value) -> McpTool {
    McpTool {
        name,
        description: tool["description"]
            .as_str()
            .unwrap_or("Lumvise assistant tool")
            .to_string(),
        input_schema,
    }
}

fn input_schema(tool: &Value) -> Value {
    tool.get("inputSchema")
        .cloned()
        .unwrap_or_else(|| json!({ "type": "object" }))
}

fn compile_validator(input_schema: &Value) -> std::result::Result<Validator, String> {
    jsonschema::draft202012::new(input_schema).map_err(|error| error.to_string())
}

fn validate_arguments(
    name: &str,
    validator: &std::result::Result<Validator, String>,
    arguments: &Value,
) -> std::result::Result<(), String> {
    let validator = validator.as_ref().map_err(|error| {
        format!(
            "MCP tool `{name}` has an invalid input schema: {error}. This tool cannot be called."
        )
    })?;
    let Some(error) = validator.iter_errors(arguments).next() else {
        return Ok(());
    };
    let instance_path = error.instance_path().as_str();
    let instance_path = if instance_path.is_empty() {
        "/"
    } else {
        instance_path
    };
    Err(format!(
        "MCP tool `{name}` received invalid arguments at `{instance_path}`: {error}. Correct the arguments to match the declared input schema and retry."
    ))
}

pub fn mcp_message_url(url: &str) -> String {
    if url.contains("/sse/") {
        return url.replacen("/sse/", "/messages/", 1);
    }
    if url.ends_with("/sse") {
        return format!("{}{}", url.trim_end_matches("/sse"), "/messages");
    }
    url.to_string()
}

fn post_mcp_json(client: &Client, provider_id: &str, url: &str, payload: Value) -> Result<Value> {
    client
        .post(url)
        .json(&payload)
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json::<Value>())
        .map_err(|error| NeuralError::ProviderFailed {
            provider_id: provider_id.to_string(),
            message: format!("MCP HTTP request to {url:?} failed: {error}"),
        })
}

fn classify_tool_call_response(provider_id: &str, response: Value) -> ToolOutcome {
    if let Some(error) = response.get("error") {
        if error.get("code").and_then(Value::as_i64) == Some(-32602) {
            return ToolOutcome::Validation(
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("MCP rejected tool arguments")
                    .to_string(),
            );
        }
        return ToolOutcome::ToolError(error.clone());
    }
    if let Some(result) = response.get("result") {
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            return ToolOutcome::ToolError(result.clone());
        }
        return ToolOutcome::Success(result.clone());
    }
    ToolOutcome::Transport(NeuralError::ProviderFailed {
        provider_id: provider_id.to_string(),
        message: format!("MCP response missing result or error: {response}"),
    })
}

#[cfg(test)]
mod tests;
