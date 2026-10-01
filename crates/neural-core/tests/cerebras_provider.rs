#[path = "cerebras_provider/support.rs"]
mod support;

use lumvise_neural_core::llm_providers::{
    LlmMcpServerConfig, LlmModalityInputKind, LlmResponseFormat, LlmStreamEvent,
};
use lumvise_neural_core::process::StreamControl;
use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, NeuralError, SpawnConfig};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;
use std::time::Duration;

use support::*;

// F-001: config validation / anchoring.

#[test]
fn cerebras_config_validates_with_endpoint_credential_and_model() {
    assert!(cerebras_config().validate().is_ok());
}

#[test]
fn cerebras_config_without_endpoint_is_rejected() {
    let mut config = cerebras_config();
    config.endpoint = None;
    assert!(config.validate().is_err());
}

#[test]
fn cerebras_config_without_credential_is_rejected() {
    let mut config = cerebras_config();
    config.credential = None;
    assert!(config.validate().is_err());
}

#[test]
fn cerebras_config_with_empty_model_is_rejected() {
    let mut config = cerebras_config();
    config.model = "  ".into();
    assert!(config.validate().is_err());
}

#[test]
fn cerebras_kind_does_not_accept_local_spawn_config() {
    let config = LlmProviderConfig {
        provider_id: "cerebras".into(),
        kind: LlmProviderKind::Cerebras,
        model: "gemma-4-31b".into(),
        endpoint: None,
        credential: None,
        completion_concurrency: None,
        spawn: Some(SpawnConfig {
            command: "echo".into(),
            args: vec![],
            timeout_ms: 1000,
        }),
    };
    assert!(config.validate().is_err());
}

// F-002: final Chat Completions payload / parse + malformed payload.

#[test]
fn cerebras_complete_posts_chat_completions_payload_and_parses_message_content() {
    let http_client = CerebrasFakeHttpClient::completion(json!({
        "choices": [{ "message": { "content": "cerebras reply" } }]
    }));
    let registry = registry_with_client(http_client.clone());

    let response = registry
        .complete("cerebras", &cerebras_request(false))
        .unwrap();

    assert_eq!(response.content, "cerebras reply");
    assert_eq!(response.provider_id, "cerebras");
    assert_eq!(http_client.calls(), vec![expected_call(false)]);
}

#[test]
fn cerebras_complete_rejects_generic_remote_payload_with_cerebras_label() {
    let http_client = CerebrasFakeHttpClient::completion(json!({
        "content": "generic remote response"
    }));
    let registry = registry_with_client(http_client.clone());

    let error = registry
        .complete("cerebras", &cerebras_request(false))
        .unwrap_err()
        .to_string();

    assert!(error.contains("Cerebras choices[0] content"));
    assert_eq!(http_client.calls(), vec![expected_call(false)]);
}

// Options (LlmRequestOptions) flow into the Chat Completions payload.

#[test]
fn cerebras_options_set_max_tokens_temperature_and_response_format() {
    let http_client = CerebrasFakeHttpClient::completion(json!({
        "choices": [{ "message": { "content": "{\"ok\":true}" } }]
    }));
    let registry = registry_with_client(http_client.clone());
    let mut request = cerebras_request(false);
    request.options.max_output_tokens = Some(512);
    request.options.temperature = Some(0.3);
    request.options.response_format = Some(LlmResponseFormat::JsonSchema {
        name: "answer".to_string(),
        schema: json!({ "type": "object" }),
        strict: true,
    });

    let response = registry.complete("cerebras", &request).unwrap();

    assert_eq!(response.content, "{\"ok\":true}");
    let call = &http_client.calls()[0];
    assert_eq!(call.payload["max_tokens"], json!(512));
    let temperature = call.payload["temperature"].as_f64().unwrap();
    assert!(
        (temperature - 0.3).abs() < 0.001,
        "temperature was {temperature}"
    );
    assert_eq!(
        call.payload["response_format"],
        json!({
            "type": "json_schema",
            "json_schema": {
                "name": "answer",
                "schema": { "type": "object" },
                "strict": true
            }
        })
    );
}

#[test]
fn cerebras_default_options_omit_optional_payload_fields() {
    let http_client = CerebrasFakeHttpClient::completion(json!({
        "choices": [{ "message": { "content": "reply" } }]
    }));
    let registry = registry_with_client(http_client.clone());

    registry
        .complete("cerebras", &cerebras_request(false))
        .unwrap();

    let payload = &http_client.calls()[0].payload;
    assert!(payload.get("max_tokens").is_none());
    assert!(payload.get("temperature").is_none());
    assert!(payload.get("response_format").is_none());
}

#[test]
fn cerebras_response_format_omitted_on_tool_rounds() {
    let mcp = FakeMcpServer::spawn(2);
    let http_client = CerebrasFakeHttpClient::completion_sequence(vec![
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": CEREBRAS_TEST_TOOL_NAME,
                            "arguments": "{\"query\":\"format\"}"
                        }
                    }]
                }
            }]
        }),
        json!({
            "choices": [{ "message": { "content": "final after tool" } }]
        }),
    ]);
    let registry = registry_with_client(http_client.clone());
    let mut request = cerebras_request(false);
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "cerebras-tools".to_string(),
        url: format!("{}/mcp/sse/builtin.tools/session-fmt", mcp.url()),
    }];
    request.options.response_format = Some(LlmResponseFormat::JsonObject);

    let response = registry.complete("cerebras", &request).unwrap();

    assert_eq!(response.content, "final after tool");
    let calls = http_client.calls();
    assert!(calls[0].payload.get("response_format").is_none());
    assert!(calls[1].payload.get("response_format").is_none());
}

// F-002c: complete_controlled bounds each HTTP round by the caller deadline.

#[test]
fn cerebras_complete_controlled_passes_remaining_timeout_to_http() {
    let http_client = CerebrasFakeHttpClient::completion(json!({
        "choices": [{ "message": { "content": "controlled reply" } }]
    }));
    let registry = registry_with_client(http_client.clone());
    let control = InvocationControl::with_deadline(Duration::from_secs(30));

    let response = registry
        .provider_handle("cerebras")
        .unwrap()
        .complete_controlled(&cerebras_request(false), &control)
        .unwrap();

    assert_eq!(response.content, "controlled reply");
    let timeout = http_client.calls()[0]
        .timeout
        .expect("controlled call carries a timeout");
    assert!(timeout <= Duration::from_secs(30) && timeout > Duration::ZERO);
}

#[test]
fn cerebras_expired_control_fails_before_http() {
    let http_client = CerebrasFakeHttpClient::completion(json!({
        "choices": [{ "message": { "content": "never" } }]
    }));
    let registry = registry_with_client(http_client.clone());
    let control = InvocationControl::with_deadline(Duration::ZERO);

    let error = registry
        .provider_handle("cerebras")
        .unwrap()
        .complete_controlled(&cerebras_request(false), &control)
        .unwrap_err();

    assert!(matches!(error, NeuralError::ProcessTimeout { .. }));
    assert!(http_client.calls().is_empty());
}

// F-003: SSE text streaming deltas / complete / cancel / malformed.

#[test]
fn cerebras_stream_parses_sse_deltas_and_done() {
    let http_client = CerebrasFakeHttpClient::stream(vec![
        stream_delta("hel"),
        stream_delta("lo"),
        "data: [DONE]\n\n".to_string(),
        stream_delta(" ignored"),
    ]);
    let registry = registry_with_client(http_client.clone());

    let events = registry
        .stream(
            "cerebras",
            &cerebras_request(true),
            StreamControl::unbounded(),
        )
        .unwrap();

    assert_eq!(events, completed_stream_events());
    assert_eq!(http_client.calls(), vec![expected_call(true)]);
}

#[test]
fn cerebras_stream_cancellation_emits_cancelled_event() {
    let http_client = CerebrasFakeHttpClient::stream(vec![
        stream_delta("hel"),
        stream_delta("lo"),
        "data: [DONE]\n\n".to_string(),
    ]);
    let registry = registry_with_client(http_client);

    let events = registry
        .stream(
            "cerebras",
            &cerebras_request(true),
            StreamControl::cancel_after(1),
        )
        .unwrap();

    assert_eq!(
        events,
        vec![
            LlmStreamEvent::ContentDelta {
                text: "hel".to_string()
            },
            LlmStreamEvent::Cancelled,
        ]
    );
}

#[test]
fn cerebras_stream_rejects_malformed_sse_delta_content_with_cerebras_label() {
    let http_client = CerebrasFakeHttpClient::stream(vec![sse_data(json!({
        "choices": [{ "delta": { "content": 42 } }]
    }))]);
    let registry = registry_with_client(http_client);

    let error = registry
        .stream(
            "cerebras",
            &cerebras_request(true),
            StreamControl::unbounded(),
        )
        .unwrap_err()
        .to_string();

    assert!(error.contains("Cerebras content text"));
}

#[test]
fn cerebras_stream_propagates_provider_error_payload() {
    let http_client = CerebrasFakeHttpClient::stream(vec![sse_data(json!({
        "error": { "message": "cerebras overloaded" }
    }))]);
    let registry = registry_with_client(http_client);

    let events = registry
        .stream(
            "cerebras",
            &cerebras_request(true),
            StreamControl::unbounded(),
        )
        .unwrap();

    assert_eq!(
        events,
        vec![LlmStreamEvent::Error {
            message: "cerebras overloaded".to_string()
        }]
    );
}

// F-004: model-dependent image gating.

#[test]
fn cerebras_gemma_vision_model_sends_image_content_parts() {
    let http_client = CerebrasFakeHttpClient::completion(json!({
        "choices": [{ "message": { "content": "vision reply" } }]
    }));
    let registry = registry_with_client(http_client.clone());

    registry
        .complete("cerebras", &cerebras_image_request("gemma-4-31b"))
        .unwrap();

    let payload = http_client.calls()[0].payload.clone();
    assert_eq!(
        payload["messages"][0]["content"][0],
        json!({ "type": "text", "text": "describe this" })
    );
    assert_eq!(
        payload["messages"][0]["content"][1]["type"],
        json!("image_url")
    );
    assert_eq!(
        payload["messages"][0]["content"][1]["image_url"]["url"],
        json!("data:image/png;base64,iVBORw==")
    );
}

#[test]
fn cerebras_non_vision_model_rejects_image_input_before_http() {
    let http_client = CerebrasFakeHttpClient::completion(json!({
        "choices": [{ "message": { "content": "ignored" } }]
    }));
    let registry = registry_with_model(http_client.clone(), "llama3.1-8b");

    let error = registry
        .complete("cerebras", &cerebras_image_request("llama3.1-8b"))
        .unwrap_err()
        .to_string();

    assert!(error.contains("image_snapshot_input"));
    assert!(error.contains("llama3.1-8b"));
    assert!(http_client.calls().is_empty());
}

#[test]
fn cerebras_request_model_override_to_text_model_rejects_image_before_http() {
    let http_client = CerebrasFakeHttpClient::completion(json!({
        "choices": [{ "message": { "content": "ignored" } }]
    }));
    let registry = registry_with_client(http_client.clone());
    let mut request = cerebras_image_request("gemma-4-31b");
    // Configured model is vision-capable, but the request overrides to a text-only model.
    request.model = Some("llama3.1-8b".to_string());

    let error = registry
        .complete("cerebras", &request)
        .unwrap_err()
        .to_string();

    assert!(error.contains("image_snapshot_input"));
    assert!(error.contains("llama3.1-8b"));
    assert!(http_client.calls().is_empty());
}

// F-005: live audio / screen-frame must fail before HTTP.

#[test]
fn cerebras_rejects_live_audio_input_before_http() {
    let error = cerebras_modality_error(LlmModalityInputKind::LiveAudioChunk);
    assert!(error.contains("live_audio_input is unsupported"));
}

#[test]
fn cerebras_rejects_screen_frame_input_before_http() {
    let error = cerebras_modality_error(LlmModalityInputKind::ScreenFrame);
    assert!(error.contains("screen_frame_broadcast_input is unsupported"));
}

// F-007: non-streaming tool turn executes the existing MCP/function-tool bridge.

#[test]
fn cerebras_tool_call_is_executed_through_tool_invoker() {
    let mcp = FakeMcpServer::spawn(2);
    let http_client = CerebrasFakeHttpClient::completion_sequence(vec![
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": CEREBRAS_TEST_TOOL_NAME,
                            "arguments": "{\"query\":\"weather\"}"
                        }
                    }]
                }
            }]
        }),
        json!({
            "choices": [{ "message": { "content": "final answer from cerebras" } }]
        }),
    ]);
    let registry = registry_with_client(http_client.clone());
    let mut request = cerebras_request(false);
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "cerebras-tools".to_string(),
        url: format!("{}/mcp/sse/builtin.tools/session-tool", mcp.url()),
    }];

    let response = registry.complete("cerebras", &request).unwrap();
    let calls = http_client.calls();

    assert_eq!(response.content, "final answer from cerebras");
    assert_eq!(response.provider_id, "cerebras");
    assert_eq!(response.metadata["tool_bridge"], json!("mcp"));
    assert_eq!(
        calls[0].payload["tools"][0]["function"]["name"],
        json!(CEREBRAS_TEST_TOOL_NAME)
    );
    assert_eq!(calls[0].payload["tool_choice"], json!("auto"));
    assert_eq!(calls[1].payload["messages"][1]["role"], json!("assistant"));
    assert_eq!(
        calls[1].payload["messages"][1]["tool_calls"][0]["function"]["name"],
        json!(CEREBRAS_TEST_TOOL_NAME)
    );
    assert_eq!(calls[1].payload["messages"][2]["role"], json!("tool"));
    assert_eq!(mcp.tool_calls(), vec![CEREBRAS_TEST_TOOL_NAME.to_string()]);
    assert_eq!(
        mcp.paths(),
        vec![
            "/mcp/messages/builtin.tools/session-tool",
            "/mcp/messages/builtin.tools/session-tool",
        ]
    );
}

#[test]
fn cerebras_tool_round_exhaustion_returns_exact_error_without_final_request() {
    let mcp = FakeMcpServer::spawn(13);
    let mut completions = (1..=12)
        .map(|round| {
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": format!("call-{round}"),
                            "type": "function",
                            "function": {
                                "name": CEREBRAS_TEST_TOOL_NAME,
                                "arguments": "{\"query\":\"canvas\"}"
                            }
                        }]
                    }
                }]
            })
        })
        .collect::<Vec<_>>();
    completions.push(json!({
        "choices": [{ "message": { "content": "Canvas updated. What next?" } }]
    }));
    let http_client = CerebrasFakeHttpClient::completion_sequence(completions);
    let registry = registry_with_client(http_client.clone());
    let mut request = cerebras_request(false);
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "cerebras-tools".to_string(),
        url: format!("{}/mcp/sse/builtin.tools/session-limit", mcp.url()),
    }];

    let error = registry
        .complete("cerebras", &request)
        .expect_err("an unfinished twelfth tool round must fail clearly");
    let calls = http_client.calls();

    assert!(matches!(
        error,
        NeuralError::ToolRoundsExhausted { ref provider_id } if provider_id == "cerebras"
    ));
    assert_eq!(calls.len(), 12);
    assert!(
        calls.iter().all(|call| {
            call.payload["tools"].is_array() && call.payload["tool_choice"] == "auto"
        })
    );
    assert_eq!(mcp.tool_calls().len(), 12);
}

#[test]
fn cerebras_tool_round_limit_rejects_native_call_in_final_round() {
    let mcp = FakeMcpServer::spawn(13);
    let mut completions = (1..=12)
        .map(|round| {
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": format!("call-{round}"),
                            "type": "function",
                            "function": {
                                "name": CEREBRAS_TEST_TOOL_NAME,
                                "arguments": "{\"query\":\"canvas\"}"
                            }
                        }]
                    }
                }]
            })
        })
        .collect::<Vec<_>>();
    // Final round: model still emits native call text instead of clean answer.
    completions.push(json!({
        "choices": [{
            "message": {
                "content": "call:builtin_assistant__canvas_apply_diff{patch:[{op:remove\",\"path:/document/elementOrder/0\"}]}"
            }
        }]
    }));
    let http_client = CerebrasFakeHttpClient::completion_sequence(completions);
    let registry = registry_with_client(http_client.clone());
    let mut request = cerebras_request(false);
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "cerebras-tools".to_string(),
        url: format!("{}/mcp/sse/builtin.tools/session-limit", mcp.url()),
    }];

    let err = registry
        .complete("cerebras", &request)
        .expect_err("tool-round exhaustion must not dispatch an extra fallback request");
    assert!(matches!(
        err,
        NeuralError::ToolRoundsExhausted { ref provider_id } if provider_id == "cerebras"
    ));
    assert_eq!(http_client.calls().len(), 12);
    assert_eq!(mcp.tool_calls().len(), 12);
}

#[test]
fn cerebras_tool_call_with_malformed_arguments_fails_with_provider_label() {
    let mcp = FakeMcpServer::spawn(1);
    let http_client = CerebrasFakeHttpClient::completion_sequence(vec![json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-bad",
                    "type": "function",
                    "function": {
                        "name": CEREBRAS_TEST_TOOL_NAME,
                        "arguments": "{not valid json"
                    }
                }]
            }
        }]
    })]);
    let registry = registry_with_client(http_client.clone());
    let mut request = cerebras_request(false);
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "cerebras-tools".to_string(),
        url: format!("{}/mcp/sse/builtin.tools/session-bad", mcp.url()),
    }];

    let error = registry
        .complete("cerebras", &request)
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("cerebras tool call JSON arguments"),
        "error was: {error}"
    );
    // Argument parsing fails before tools/call is dispatched; only tools/list hit MCP.
    assert!(mcp.tool_calls().is_empty());
    assert_eq!(http_client.calls().len(), 1);
}

// F-009: Cerebras/Gemma can emit a tool call in Gemma's own native
// (non-JSON) text serialization instead of a structured `tool_calls`
// response; the shared recovery path dispatches it identically to a real
// structured call, and never delivers an unresolvable attempt as if it
// were an ordinary answer.

#[test]
fn cerebras_recovers_a_gemma_native_tool_call_never_seen_as_structured_tool_calls() {
    let mcp = FakeMcpServer::spawn(2);
    let http_client = CerebrasFakeHttpClient::completion_sequence(vec![
        json!({
            "choices": [{ "message": {
                "content": format!("call:{CEREBRAS_TEST_TOOL_NAME}{{query:weather}}"),
            } }]
        }),
        json!({
            "choices": [{ "message": { "content": "final answer from cerebras" } }]
        }),
    ]);
    let registry = registry_with_client(http_client.clone());
    let mut request = cerebras_request(false);
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "cerebras-tools".to_string(),
        url: format!("{}/mcp/sse/builtin.tools/session-native", mcp.url()),
    }];

    let response = registry.complete("cerebras", &request).unwrap();

    assert_eq!(response.content, "final answer from cerebras");
    assert_eq!(mcp.tool_calls(), vec![CEREBRAS_TEST_TOOL_NAME.to_string()]);
    let calls = http_client.calls();
    assert_eq!(calls[1].payload["messages"][1]["role"], json!("assistant"));
    assert_eq!(
        calls[1].payload["messages"][1]["tool_calls"][0]["function"]["name"],
        json!(CEREBRAS_TEST_TOOL_NAME)
    );
    assert_eq!(calls[1].payload["messages"][2]["role"], json!("tool"));
}

#[test]
fn cerebras_never_delivers_a_malformed_native_tool_call_attempt() {
    let mcp = FakeMcpServer::spawn(1);
    let http_client = CerebrasFakeHttpClient::completion_sequence(vec![json!({
        "choices": [{ "message": {
            "content": format!("call:{CEREBRAS_TEST_TOOL_NAME}{{query:"),
        } }]
    })]);
    let registry = registry_with_client(http_client.clone());
    let mut request = cerebras_request(false);
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "cerebras-tools".to_string(),
        url: format!("{}/mcp/sse/builtin.tools/session-native-bad", mcp.url()),
    }];

    let error = registry.complete("cerebras", &request).unwrap_err();

    assert!(
        matches!(
            error,
            lumvise_neural_core::NeuralError::MalformedPayload { .. }
        ),
        "error was: {error}"
    );
    // Detection fails before any dispatch; only tools/list hit MCP.
    assert!(mcp.tool_calls().is_empty());
    assert_eq!(http_client.calls().len(), 1);
}

// F-008: streaming requests with tools run the tool loop and emit the final answer
// as stream events (current project behavior; not interleaved streaming tool calls).

#[test]
fn cerebras_streaming_with_tools_emits_final_answer_events() {
    let mcp = FakeMcpServer::spawn(2);
    let http_client = CerebrasFakeHttpClient::completion_sequence(vec![
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": CEREBRAS_TEST_TOOL_NAME,
                            "arguments": "{\"query\":\"stream\"}"
                        }
                    }]
                }
            }]
        }),
        json!({
            "choices": [{ "message": { "content": "streamed final answer" } }]
        }),
    ]);
    let registry = registry_with_client(http_client.clone());
    let mut request = cerebras_request(true);
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "cerebras-tools".to_string(),
        url: format!("{}/mcp/sse/builtin.tools/session-stream", mcp.url()),
    }];

    let events = registry
        .stream("cerebras", &request, StreamControl::unbounded())
        .unwrap();

    assert_eq!(
        events,
        vec![
            LlmStreamEvent::ContentDelta {
                text: "streamed final answer".to_string()
            },
            LlmStreamEvent::Complete,
        ]
    );
    assert_eq!(mcp.tool_calls(), vec![CEREBRAS_TEST_TOOL_NAME.to_string()]);
}

#[test]
fn cerebras_streaming_with_tools_reports_malformed_arguments_error() {
    let mcp = FakeMcpServer::spawn(1);
    let http_client = CerebrasFakeHttpClient::completion_sequence(vec![json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-bad",
                    "type": "function",
                    "function": {
                        "name": CEREBRAS_TEST_TOOL_NAME,
                        "arguments": "{broken"
                    }
                }]
            }
        }]
    })]);
    let registry = registry_with_client(http_client.clone());
    let mut request = cerebras_request(true);
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "cerebras-tools".to_string(),
        url: format!("{}/mcp/sse/builtin.tools/session-bad-stream", mcp.url()),
    }];

    let error = registry
        .stream("cerebras", &request, StreamControl::unbounded())
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("cerebras tool call JSON arguments"),
        "error was: {error}"
    );
    assert!(mcp.tool_calls().is_empty());
}
