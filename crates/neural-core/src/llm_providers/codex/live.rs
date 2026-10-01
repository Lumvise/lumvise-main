use crate::config::LlmProviderConfig;
use crate::error::{NeuralError, Result};
use crate::llm_providers::capabilities::openai_realtime_provider_capabilities;
use crate::llm_providers::command_runner::{prompt_text, response, selected_model};
use crate::llm_providers::contract::{LlmProvider, LlmStreamEventSink};
use crate::llm_providers::local::validate_llm_request;
use crate::llm_providers::realtime::{
    RealtimeSocket, configure_socket_timeouts as configure_realtime_socket_timeouts,
    read_socket_message, send_socket_json,
};
use crate::llm_providers::tool_invocation::{
    McpToolCatalog, schema_adapters::to_openai_function_tool, tool_outcome_value,
};
use crate::llm_providers::{
    LlmMcpServerConfig, LlmModalityInputKind, LlmProviderCapabilities, LlmRequest, LlmResponse,
    LlmStreamEvent,
};
use crate::process::StreamControl;
use base64::Engine;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::header::AUTHORIZATION;
use tungstenite::http::{HeaderName, HeaderValue, Request};
use tungstenite::{Message, connect};

pub const DEFAULT_OPENAI_REALTIME_ENDPOINT: &str = "wss://api.openai.com/v1/realtime";
const OPENAI_REALTIME_SOCKET_TIMEOUT: Duration = Duration::from_secs(30);
type OpenAiRealtimeSocket = RealtimeSocket;

pub trait OpenAiRealtimeClient: Send + Sync {
    /// Streams one OpenAI Realtime turn through a client-owned transport.
    ///
    /// # Example
    ///
    /// Implementors forward `turn.inputs` to Realtime and call `on_event` for
    /// each tracked text event produced by the model.
    fn stream_turn(
        &self,
        turn: OpenAiRealtimeTurn,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiRealtimeTurn {
    pub provider_id: String,
    pub model: String,
    pub endpoint: String,
    pub credential: String,
    pub user_text: String,
    pub mcp_servers: Vec<LlmMcpServerConfig>,
    pub inputs: Vec<OpenAiRealtimeInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiRealtimeInput {
    pub kind: LlmModalityInputKind,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

pub struct OpenAiRealtimeProvider {
    config: LlmProviderConfig,
    client: Arc<dyn OpenAiRealtimeClient>,
}

impl OpenAiRealtimeProvider {
    /// Creates an OpenAI Realtime provider with the production client boundary.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let provider = OpenAiRealtimeProvider::new(config)?;
    /// ```
    pub fn new(config: LlmProviderConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            client: Arc::new(OpenAiRealtimeWebSocketClient),
        })
    }

    pub fn new_with_client(
        config: LlmProviderConfig,
        client: Arc<dyn OpenAiRealtimeClient>,
    ) -> Result<Self> {
        config.validate()?;
        Ok(Self { config, client })
    }
}

impl LlmProvider for OpenAiRealtimeProvider {
    fn run_audio_session(
        &self,
        request: crate::llm_providers::AudioSessionRequest,
        input: crate::llm_providers::AudioSessionInput,
        sink: &mut crate::llm_providers::AudioSessionEventSink<'_>,
    ) -> Result<()> {
        crate::llm_providers::realtime::audio_connection::run_openai(
            &self.config,
            request,
            input,
            sink,
        )
    }

    fn provider_id(&self) -> &str {
        &self.config.provider_id
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        openai_realtime_provider_capabilities(&self.config.provider_id)
    }

    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        validate_llm_request(request)?;
        let mut content = String::new();
        self.stream_turn(request, StreamControl::unbounded(), &mut |event| {
            append_text_event(&mut content, event)
        })?;
        Ok(response(
            &self.config,
            request,
            content,
            json!({ "transport": "openai_realtime" }),
        ))
    }

    fn stream_with_events(
        &self,
        request: &LlmRequest,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        validate_llm_request(request)?;
        self.stream_turn(request, control, on_event)
    }
}

impl OpenAiRealtimeProvider {
    fn stream_turn(
        &self,
        request: &LlmRequest,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        self.client.stream_turn(
            openai_realtime_turn(&self.config, request)?,
            control,
            on_event,
        )
    }
}

struct OpenAiRealtimeWebSocketClient;

impl OpenAiRealtimeClient for OpenAiRealtimeWebSocketClient {
    fn stream_turn(
        &self,
        turn: OpenAiRealtimeTurn,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        let mcp_tools = OpenAiRealtimeMcpTools::discover(&turn)?;
        let request = openai_realtime_connect_request(&turn)?;
        let (mut socket, _) =
            connect(request).map_err(|source| realtime_transport_error(&turn, source))?;
        configure_realtime_socket(&mut socket, &turn)?;
        send_realtime_json(&mut socket, session_update_event(&turn, &mcp_tools))?;
        send_realtime_inputs(&mut socket, &turn)?;
        send_realtime_json(&mut socket, response_create_event())?;
        read_realtime_events(&mut socket, &turn, control, &mcp_tools, on_event)
    }
}

pub(crate) fn openai_realtime_turn(
    config: &LlmProviderConfig,
    request: &LlmRequest,
) -> Result<OpenAiRealtimeTurn> {
    Ok(OpenAiRealtimeTurn {
        provider_id: config.provider_id.clone(),
        model: selected_model(config, request),
        endpoint: openai_realtime_endpoint(config),
        credential: openai_realtime_credential(config)?,
        user_text: prompt_text(request),
        mcp_servers: request.mcp_servers.clone(),
        inputs: openai_realtime_inputs(request),
    })
}

fn openai_realtime_endpoint(config: &LlmProviderConfig) -> String {
    config
        .endpoint
        .clone()
        .unwrap_or_else(|| DEFAULT_OPENAI_REALTIME_ENDPOINT.to_string())
}

fn openai_realtime_credential(config: &LlmProviderConfig) -> Result<String> {
    let credential = config.credential.as_deref().unwrap_or_default().trim();
    if credential.is_empty() {
        return Err(NeuralError::MissingValue {
            value: "openai realtime credential".to_string(),
            expected: "non-empty OpenAI API key for Realtime provider".to_string(),
        });
    }
    Ok(credential.to_string())
}

fn openai_realtime_inputs(request: &LlmRequest) -> Vec<OpenAiRealtimeInput> {
    request
        .modality_inputs
        .iter()
        .map(|input| OpenAiRealtimeInput {
            kind: input.kind,
            media_type: input.media_type.clone(),
            bytes: input.bytes.clone(),
        })
        .collect()
}

struct OpenAiRealtimeMcpTools {
    tools: McpToolCatalog,
    function_tools: Vec<Value>,
}

struct OpenAiRealtimeFunctionCall {
    call_id: String,
    name: String,
    arguments: Value,
}

impl OpenAiRealtimeMcpTools {
    fn discover(turn: &OpenAiRealtimeTurn) -> Result<Self> {
        let tools = McpToolCatalog::discover(&turn.provider_id, &turn.mcp_servers)?;
        let function_tools = tools.tools().iter().map(to_openai_function_tool).collect();
        Ok(Self {
            tools,
            function_tools,
        })
    }

    fn execute_call(
        &self,
        turn: &OpenAiRealtimeTurn,
        call: OpenAiRealtimeFunctionCall,
    ) -> Result<Value> {
        let _ = turn;
        let result = tool_outcome_value(self.tools.invoker().invoke(&call.name, call.arguments))?;
        Ok(json!({
            "type": "conversation.item.create",
            "item": {
                "type": "function_call_output",
                "call_id": call.call_id,
                "output": serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string())
            }
        }))
    }
}

pub(crate) fn openai_realtime_connect_request(turn: &OpenAiRealtimeTurn) -> Result<Request<()>> {
    let mut request = realtime_endpoint_for_model(&turn.endpoint, &turn.model)
        .into_client_request()
        .map_err(|source| NeuralError::InvalidValue {
            value: turn.endpoint.clone(),
            expected: format!("valid OpenAI Realtime WebSocket URL: {source}"),
        })?;
    let headers = request.headers_mut();
    headers.insert(AUTHORIZATION, bearer_header(&turn.credential)?);
    headers.insert(
        HeaderName::from_static("openai-beta"),
        HeaderValue::from_static("realtime=v1"),
    );
    Ok(request)
}

fn realtime_endpoint_for_model<'a>(endpoint: &'a str, model: &str) -> Cow<'a, str> {
    if endpoint.contains("model=") {
        return Cow::Borrowed(endpoint);
    }
    let separator = if endpoint.contains('?') { '&' } else { '?' };
    Cow::Owned(format!("{endpoint}{separator}model={model}"))
}

fn bearer_header(credential: &str) -> Result<HeaderValue> {
    HeaderValue::from_str(&format!("Bearer {credential}")).map_err(|source| {
        NeuralError::InvalidValue {
            value: "openai realtime credential".to_string(),
            expected: format!("valid authorization header: {source}"),
        }
    })
}

fn configure_realtime_socket(
    socket: &mut OpenAiRealtimeSocket,
    turn: &OpenAiRealtimeTurn,
) -> Result<()> {
    configure_realtime_socket_timeouts(socket, Some(OPENAI_REALTIME_SOCKET_TIMEOUT), |_, source| {
        NeuralError::Io {
            value: format!(
                "openai realtime websocket timeout for model `{}`",
                turn.model
            ),
            expected: format!(
                "timeout set to {}s",
                OPENAI_REALTIME_SOCKET_TIMEOUT.as_secs()
            ),
            source,
        }
    })
}

fn session_update_event(turn: &OpenAiRealtimeTurn, tools: &OpenAiRealtimeMcpTools) -> Value {
    json!({
        "type": "session.update",
        "session": {
            "modalities": ["text"],
            "instructions": openai_realtime_instructions(turn),
            "tools": tools.function_tools,
            "tool_choice": "auto"
        }
    })
}

fn openai_realtime_instructions(turn: &OpenAiRealtimeTurn) -> String {
    if turn.mcp_servers.is_empty() {
        return "Respond with concise text.".to_string();
    }
    "Use the provided functions for Lumvise assistant app actions before answering when needed."
        .to_string()
}

fn send_realtime_inputs(
    socket: &mut OpenAiRealtimeSocket,
    turn: &OpenAiRealtimeTurn,
) -> Result<()> {
    for input in &turn.inputs {
        if input.kind == LlmModalityInputKind::LiveAudioChunk {
            send_realtime_json(socket, audio_append_event(input))?;
        }
    }
    if turn
        .inputs
        .iter()
        .any(|input| input.kind == LlmModalityInputKind::LiveAudioChunk)
    {
        send_realtime_json(socket, json!({ "type": "input_audio_buffer.commit" }))?;
    }
    send_realtime_json(socket, user_message_event(turn))
}

fn user_message_event(turn: &OpenAiRealtimeTurn) -> Value {
    let mut content = vec![json!({ "type": "input_text", "text": turn.user_text })];
    content.extend(turn.inputs.iter().filter_map(image_content_part));
    json!({
        "type": "conversation.item.create",
        "item": { "type": "message", "role": "user", "content": content }
    })
}

fn image_content_part(input: &OpenAiRealtimeInput) -> Option<Value> {
    if input.kind != LlmModalityInputKind::ScreenFrame {
        return None;
    }
    Some(json!({
        "type": "input_image",
        "image_url": data_url(&input.media_type, &input.bytes)
    }))
}

fn audio_append_event(input: &OpenAiRealtimeInput) -> Value {
    json!({
        "type": "input_audio_buffer.append",
        "audio": base64::engine::general_purpose::STANDARD.encode(&input.bytes)
    })
}

fn response_create_event() -> Value {
    json!({ "type": "response.create", "response": { "modalities": ["text"] } })
}

fn read_realtime_events(
    socket: &mut OpenAiRealtimeSocket,
    turn: &OpenAiRealtimeTurn,
    control: StreamControl,
    tools: &OpenAiRealtimeMcpTools,
    on_event: &mut LlmStreamEventSink<'_>,
) -> Result<()> {
    let mut emitted = 0usize;
    loop {
        let value = read_realtime_json(socket, turn)?;
        fail_on_realtime_error(turn, &value)?;
        if let Some(call) = openai_function_call(&value)? {
            send_realtime_json(socket, tools.execute_call(turn, call)?)?;
            send_realtime_json(socket, response_create_event())?;
            continue;
        }
        for text in realtime_text_deltas(&value) {
            emitted += 1;
            on_event(LlmStreamEvent::ContentDelta { text })?;
            if control.should_cancel_after(emitted) {
                return on_event(LlmStreamEvent::Cancelled);
            }
        }
        if realtime_response_is_done(&value) {
            return on_event(LlmStreamEvent::Complete);
        }
    }
}

fn read_realtime_json(
    socket: &mut OpenAiRealtimeSocket,
    turn: &OpenAiRealtimeTurn,
) -> Result<Value> {
    match read_socket_message(socket, |source| realtime_transport_error(turn, source))? {
        Message::Text(text) => parse_realtime_json(&text),
        Message::Binary(bytes) => parse_realtime_bytes(&bytes),
        Message::Close(frame) => Err(NeuralError::ProviderFailed {
            provider_id: turn.provider_id.clone(),
            message: format!("OpenAI Realtime closed before response.done: {frame:?}"),
        }),
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => unreachable!(),
    }
}

fn parse_realtime_bytes(bytes: &[u8]) -> Result<Value> {
    let text = std::str::from_utf8(bytes).map_err(|source| NeuralError::InvalidValue {
        value: "openai realtime binary websocket message".to_string(),
        expected: format!("valid UTF-8 websocket text: {source}"),
    })?;
    parse_realtime_json(text)
}

fn parse_realtime_json(text: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|source| NeuralError::Json {
        value: text.to_string(),
        expected: "OpenAI Realtime server JSON message".to_string(),
        source,
    })
}

fn send_realtime_json(socket: &mut OpenAiRealtimeSocket, value: Value) -> Result<()> {
    send_socket_json(
        socket,
        value,
        "openai realtime websocket message",
        "serializable JSON value",
        |source| NeuralError::Io {
            value: "openai realtime websocket send".to_string(),
            expected: "successful websocket write".to_string(),
            source: std::io::Error::other(source.to_string()),
        },
    )
}

fn openai_function_call(value: &Value) -> Result<Option<OpenAiRealtimeFunctionCall>> {
    if value["type"] == json!("response.function_call_arguments.done") {
        return Ok(Some(OpenAiRealtimeFunctionCall {
            call_id: value["call_id"].as_str().unwrap_or_default().to_string(),
            name: required_event_string(value, "name")?,
            arguments: parse_function_arguments(value["arguments"].as_str().unwrap_or("{}"))?,
        }));
    }
    let item = value.get("item").unwrap_or(value);
    if item["type"] != json!("function_call") {
        return Ok(None);
    }
    Ok(Some(OpenAiRealtimeFunctionCall {
        call_id: required_event_string(item, "call_id")?,
        name: required_event_string(item, "name")?,
        arguments: parse_function_arguments(item["arguments"].as_str().unwrap_or("{}"))?,
    }))
}

fn required_event_string(value: &Value, key: &str) -> Result<String> {
    value[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| NeuralError::InvalidValue {
            value: value.to_string(),
            expected: format!("OpenAI Realtime event with string `{key}`"),
        })
}

fn parse_function_arguments(raw: &str) -> Result<Value> {
    serde_json::from_str(raw).map_err(|source| NeuralError::Json {
        value: raw.to_string(),
        expected: "OpenAI Realtime function call JSON arguments".to_string(),
        source,
    })
}

fn realtime_text_deltas(value: &Value) -> Vec<String> {
    match value["type"].as_str() {
        Some("response.text.delta")
        | Some("response.output_text.delta")
        | Some("response.audio_transcript.delta") => value["delta"]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(|text| vec![text.to_string()])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn realtime_response_is_done(value: &Value) -> bool {
    matches!(
        value["type"].as_str(),
        Some("response.done" | "response.completed")
    )
}

fn fail_on_realtime_error(turn: &OpenAiRealtimeTurn, value: &Value) -> Result<()> {
    if value["type"] != json!("error") {
        return Ok(());
    }
    Err(NeuralError::ProviderFailed {
        provider_id: turn.provider_id.clone(),
        message: format!("OpenAI Realtime returned error: {value}"),
    })
}

fn realtime_transport_error(turn: &OpenAiRealtimeTurn, source: tungstenite::Error) -> NeuralError {
    NeuralError::ProviderFailed {
        provider_id: turn.provider_id.clone(),
        message: format!(
            "OpenAI Realtime websocket failed for model `{}`: {source}",
            turn.model
        ),
    }
}

fn data_url(media_type: &str, bytes: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:{media_type};base64,{encoded}")
}

fn append_text_event(content: &mut String, event: LlmStreamEvent) -> Result<()> {
    match event {
        LlmStreamEvent::Session { .. } => Ok(()),
        LlmStreamEvent::ContentDelta { text } | LlmStreamEvent::FinalText { text } => {
            content.push_str(&text);
            Ok(())
        }
        LlmStreamEvent::Complete => Ok(()),
        LlmStreamEvent::Cancelled => Err(NeuralError::ProviderFailed {
            provider_id: "openai_realtime".to_string(),
            message: "OpenAI Realtime completion was cancelled".to_string(),
        }),
        LlmStreamEvent::Error { message } => Err(NeuralError::ProviderFailed {
            provider_id: "openai_realtime".to_string(),
            message,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LlmProviderKind;
    use crate::llm_providers::{LlmMessage, LlmModalityInput};
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Mutex;
    use std::thread::{self, JoinHandle};
    use tungstenite::WebSocket;

    const OPENAI_REALTIME_TOOL_CALL: &str = r#"{"type":"response.function_call_arguments.done","call_id":"call-1","name":"builtin_assistant__assistant_respond","arguments":"{\"content\":\"hi from realtime tool\"}"}"#;

    #[derive(Default)]
    struct FakeOpenAiRealtimeClient {
        turns: Mutex<Vec<OpenAiRealtimeTurn>>,
    }

    impl OpenAiRealtimeClient for FakeOpenAiRealtimeClient {
        fn stream_turn(
            &self,
            turn: OpenAiRealtimeTurn,
            control: StreamControl,
            on_event: &mut LlmStreamEventSink<'_>,
        ) -> Result<()> {
            self.turns.lock().unwrap().push(turn);
            on_event(LlmStreamEvent::ContentDelta {
                text: "openai ".to_string(),
            })?;
            if control.should_cancel_after(1) {
                return on_event(LlmStreamEvent::Cancelled);
            }
            on_event(LlmStreamEvent::ContentDelta {
                text: "realtime".to_string(),
            })?;
            on_event(LlmStreamEvent::Complete)
        }
    }

    #[test]
    fn routes_live_audio_and_screen_frames_to_realtime_client() {
        let fake = Arc::new(FakeOpenAiRealtimeClient::default());
        let provider = provider_with_client(fake.clone());

        let events = provider
            .stream(&live_request(), StreamControl::unbounded())
            .unwrap();

        assert_eq!(events.last(), Some(&LlmStreamEvent::Complete));
        let turns = fake.turns.lock().unwrap();
        assert_eq!(turns[0].provider_id, "openai_realtime");
        assert_eq!(turns[0].inputs.len(), 2);
        assert_eq!(
            turns[0].inputs[0].kind,
            LlmModalityInputKind::LiveAudioChunk
        );
        assert_eq!(turns[0].inputs[1].kind, LlmModalityInputKind::ScreenFrame);
    }

    #[test]
    fn complete_collects_realtime_text_deltas() {
        let provider = provider_with_client(Arc::new(FakeOpenAiRealtimeClient::default()));

        let response = provider.complete(&live_request()).unwrap();

        assert_eq!(response.content, "openai realtime");
        assert_eq!(response.metadata["transport"], "openai_realtime");
    }

    #[test]
    fn forwards_assistant_mcp_servers_to_realtime_client() {
        let fake = Arc::new(FakeOpenAiRealtimeClient::default());
        let provider = provider_with_client(fake.clone());

        provider
            .stream(&mcp_request(), StreamControl::unbounded())
            .unwrap();

        let turns = fake.turns.lock().unwrap();
        assert_eq!(turns[0].mcp_servers.len(), 1);
        assert_eq!(turns[0].mcp_servers[0].name, "lumvise-assistant");
        assert_eq!(
            turns[0].mcp_servers[0].url,
            "http://127.0.0.1:4180/mcp/sse/builtin.assistant/session-a"
        );
    }

    #[test]
    fn cancellation_returns_cancelled_without_fake_complete() {
        let provider = provider_with_client(Arc::new(FakeOpenAiRealtimeClient::default()));

        let events = provider
            .stream(&live_request(), StreamControl::cancel_after(1))
            .unwrap();

        assert_eq!(events.last(), Some(&LlmStreamEvent::Cancelled));
        assert!(!events.contains(&LlmStreamEvent::Complete));
    }

    #[test]
    fn production_client_executes_mcp_tool_call_over_realtime_socket() {
        let mcp = FakeOpenAiRealtimeMcpServer::spawn();
        let server = start_realtime_tool_call_server();
        let provider = OpenAiRealtimeProvider::new(config_with_endpoint(server.endpoint)).unwrap();
        let request = text_request_with_mcp(mcp.url());

        let events = provider
            .stream(&request, StreamControl::unbounded())
            .unwrap();
        let socket_messages = server.handle.join().unwrap();

        assert_eq!(
            events,
            vec![
                LlmStreamEvent::ContentDelta {
                    text: "realtime ok".to_string()
                },
                LlmStreamEvent::Complete,
            ]
        );
        assert!(socket_messages[0].contains("builtin_assistant__assistant_respond"));
        assert!(socket_messages[3].contains("function_call_output"));
        assert!(socket_messages[3].contains("call-1"));
        assert_eq!(
            mcp.tool_calls(),
            vec!["builtin_assistant__assistant_respond"]
        );
        assert_eq!(
            mcp.paths(),
            vec![
                "/mcp/messages/builtin.assistant/session-a",
                "/mcp/messages/builtin.assistant/session-a"
            ]
        );
    }

    fn provider_with_client(client: Arc<dyn OpenAiRealtimeClient>) -> OpenAiRealtimeProvider {
        OpenAiRealtimeProvider::new_with_client(config(), client).unwrap()
    }

    fn config() -> LlmProviderConfig {
        LlmProviderConfig {
            provider_id: "openai_realtime".to_string(),
            kind: LlmProviderKind::OpenAiRealtime,
            model: "gpt-realtime".to_string(),
            endpoint: Some(DEFAULT_OPENAI_REALTIME_ENDPOINT.to_string()),
            credential: Some("test-key".to_string()),
            completion_concurrency: None,
            spawn: None,
        }
    }

    fn config_with_endpoint(endpoint: String) -> LlmProviderConfig {
        LlmProviderConfig {
            endpoint: Some(endpoint),
            ..config()
        }
    }

    fn live_request() -> LlmRequest {
        LlmRequest {
            modality_inputs: vec![live_audio_input(), screen_frame_input()],
            ..text_request()
        }
    }

    fn text_request() -> LlmRequest {
        LlmRequest {
            options: Default::default(),
            messages: vec![LlmMessage {
                role: "user".to_string(),
                content: "hello live".to_string(),
            }],
            stream: true,
            provider_id: Some("openai_realtime".to_string()),
            model: None,
            conversation_id: None,
            provider_session_id: None,
            mcp_servers: Vec::new(),
            modality_inputs: Vec::new(),
        }
    }

    fn mcp_request() -> LlmRequest {
        let mut request = text_request();
        request.mcp_servers = vec![LlmMcpServerConfig {
            name: "lumvise-assistant".to_string(),
            url: "http://127.0.0.1:4180/mcp/sse/builtin.assistant/session-a".to_string(),
        }];
        request
    }

    fn text_request_with_mcp(mcp_url: String) -> LlmRequest {
        let mut request = text_request();
        request.mcp_servers = vec![LlmMcpServerConfig {
            name: "lumvise-assistant".to_string(),
            url: format!("{mcp_url}/mcp/sse/builtin.assistant/session-a"),
        }];
        request
    }

    fn live_audio_input() -> LlmModalityInput {
        LlmModalityInput {
            input_id: "audio".to_string(),
            kind: LlmModalityInputKind::LiveAudioChunk,
            media_type: "audio/pcm;rate=16000".to_string(),
            bytes: vec![1, 2],
            metadata: json!({}),
        }
    }

    fn screen_frame_input() -> LlmModalityInput {
        LlmModalityInput {
            input_id: "screen".to_string(),
            kind: LlmModalityInputKind::ScreenFrame,
            media_type: "image/jpeg".to_string(),
            bytes: vec![3, 4],
            metadata: json!({}),
        }
    }

    struct RealtimeTestServer {
        endpoint: String,
        handle: JoinHandle<Vec<String>>,
    }

    struct FakeOpenAiRealtimeMcpServer {
        url: String,
        calls: Arc<Mutex<Vec<String>>>,
        paths: Arc<Mutex<Vec<String>>>,
        _thread: JoinHandle<()>,
    }

    impl FakeOpenAiRealtimeMcpServer {
        fn spawn() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let calls = Arc::new(Mutex::new(Vec::new()));
            let paths = Arc::new(Mutex::new(Vec::new()));
            let thread_calls = Arc::clone(&calls);
            let thread_paths = Arc::clone(&paths);
            let handle = thread::spawn(move || {
                for _ in 0..2 {
                    let (stream, _) = listener.accept().unwrap();
                    handle_openai_mcp_connection(stream, &thread_calls, &thread_paths);
                }
            });
            Self {
                url,
                calls,
                paths,
                _thread: handle,
            }
        }

        fn url(&self) -> String {
            self.url.clone()
        }

        fn tool_calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn paths(&self) -> Vec<String> {
            self.paths.lock().unwrap().clone()
        }
    }

    fn start_realtime_tool_call_server() -> RealtimeTestServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}/realtime", listener.local_addr().unwrap());
        let handle = thread::spawn(move || run_realtime_tool_call_server(listener));
        RealtimeTestServer { endpoint, handle }
    }

    fn run_realtime_tool_call_server(listener: TcpListener) -> Vec<String> {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        let mut messages = Vec::new();
        receive_realtime_bootstrap(&mut socket, &mut messages);
        send_realtime_server_json(&mut socket, OPENAI_REALTIME_TOOL_CALL);
        messages.push(read_realtime_client_text(&mut socket));
        messages.push(read_realtime_client_text(&mut socket));
        send_realtime_server_json(
            &mut socket,
            r#"{"type":"response.output_text.delta","delta":"realtime ok"}"#,
        );
        send_realtime_server_json(
            &mut socket,
            r#"{"type":"response.done","response":{"status":"completed"}}"#,
        );
        messages
    }

    fn receive_realtime_bootstrap(socket: &mut WebSocket<TcpStream>, messages: &mut Vec<String>) {
        let session = read_realtime_client_text(socket);
        assert_text_contains(
            &session,
            &["session.update", "builtin_assistant__assistant_respond"],
        );
        messages.push(session);
        let input = read_realtime_client_text(socket);
        assert_text_contains(&input, &["conversation.item.create", "hello live"]);
        messages.push(input);
        let create = read_realtime_client_text(socket);
        assert_text_contains(&create, &["response.create"]);
        messages.push(create);
    }

    fn read_realtime_client_text(socket: &mut WebSocket<TcpStream>) -> String {
        socket.read().unwrap().into_text().unwrap().to_string()
    }

    fn send_realtime_server_json(socket: &mut WebSocket<TcpStream>, text: &str) {
        socket.send(Message::Text(text.to_string().into())).unwrap();
    }

    fn handle_openai_mcp_connection(
        mut stream: TcpStream,
        calls: &Arc<Mutex<Vec<String>>>,
        paths: &Arc<Mutex<Vec<String>>>,
    ) {
        let mut buffer = [0; 8192];
        let len = stream.read(&mut buffer).unwrap();
        let request = String::from_utf8_lossy(&buffer[..len]);
        paths.lock().unwrap().push(http_path_from_request(&request));
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("{}");
        let payload: Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
        write_http_json(&mut stream, openai_mcp_response(&payload, calls));
    }

    fn http_path_from_request(request: &str) -> String {
        request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string()
    }

    fn openai_mcp_response(payload: &Value, calls: &Arc<Mutex<Vec<String>>>) -> Value {
        if payload["method"] == json!("tools/list") {
            return openai_mcp_tools_response();
        }
        let name = payload["params"]["name"].as_str().unwrap_or_default();
        calls.lock().unwrap().push(name.to_string());
        json!({
            "jsonrpc": "2.0",
            "id": payload["id"].clone(),
            "result": { "ok": true }
        })
    }

    fn openai_mcp_tools_response() -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "tools": [openai_assistant_tool()] }
        })
    }

    fn openai_assistant_tool() -> Value {
        json!({
            "name": "builtin_assistant__assistant_respond",
            "description": "Respond to the user",
            "inputSchema": {
                "type": "object",
                "properties": { "content": { "type": "string" } },
                "required": ["content"]
            }
        })
    }

    fn write_http_json(stream: &mut TcpStream, value: Value) {
        let body = value.to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
    }

    fn assert_text_contains(text: &str, expected_parts: &[&str]) {
        for expected in expected_parts {
            assert!(
                text.contains(expected),
                "expected `{text}` to contain `{expected}`"
            );
        }
    }
}
