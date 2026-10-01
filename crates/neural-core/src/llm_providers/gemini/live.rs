use crate::error::{NeuralError, Result};
use crate::llm_providers::contract::LlmStreamEventSink;
use crate::llm_providers::realtime::{
    RealtimeSocket, configure_socket_timeouts as configure_realtime_socket_timeouts,
    read_socket_message, send_socket_json,
};
use crate::llm_providers::tool_invocation::{
    McpToolCatalog, schema_adapters::to_gemini_declaration, tool_outcome_value,
};
use crate::llm_providers::{LlmMcpServerConfig, LlmModalityInputKind, LlmStreamEvent};
use crate::process::StreamControl;
use base64::Engine;
use serde::Serialize;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::io::ErrorKind;
use std::sync::Arc;
use std::time::Duration;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::header::AUTHORIZATION;
use tungstenite::http::{HeaderValue, Request};
use tungstenite::protocol::frame::CloseFrame;
use tungstenite::{Message, connect};

pub const DEFAULT_GEMINI_LIVE_ENDPOINT: &str = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";
pub const DEFAULT_GEMINI_LIVE_CONSTRAINED_ENDPOINT: &str = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1alpha.GenerativeService.BidiGenerateContentConstrained";
const GEMINI_LIVE_SOCKET_TIMEOUT: Duration = Duration::from_secs(30);
type GeminiLiveSocket = RealtimeSocket;
pub type GeminiLiveTraceSink = dyn Fn(GeminiLiveTraceEvent) + Send + Sync;

pub trait GeminiLiveTransport: Send + Sync {
    /// Streams one Gemini Live turn through the provider-owned transport.
    ///
    /// # Example
    ///
    /// Implementors send `turn.inputs` as Live API realtime messages and call
    /// `on_event` for each received text delta.
    fn stream_turn(
        &self,
        turn: GeminiLiveTurn,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiLiveTurn {
    pub provider_id: String,
    pub model: String,
    pub credential: GeminiLiveCredential,
    pub session_resumption_handle: Option<String>,
    pub user_text: String,
    pub mcp_servers: Vec<LlmMcpServerConfig>,
    pub inputs: Vec<GeminiLiveInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiLiveCredential {
    ApiKey(String),
    OAuthBearer(String),
    EphemeralToken(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiLiveInput {
    pub kind: LlmModalityInputKind,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GeminiLiveTraceEvent {
    pub stage: GeminiLiveTraceStage,
    pub media_type: Option<String>,
    pub bytes: Option<usize>,
    pub text_chars: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GeminiLiveTraceStage {
    WebsocketConnected,
    SetupSent,
    SetupCompleteReceived,
    RealtimeTextSent,
    RealtimeAudioSent,
    RealtimeVideoSent,
    AudioStreamEndSent,
    TextDeltaReceived,
    TurnCompleteReceived,
    Cancelled,
    TransportTimeout,
}

#[derive(Clone)]
pub struct GeminiLiveWebSocketTransport {
    endpoint: String,
    trace_sink: Option<Arc<GeminiLiveTraceSink>>,
}

impl std::fmt::Debug for GeminiLiveWebSocketTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GeminiLiveWebSocketTransport")
            .field("endpoint", &self.endpoint)
            .field("trace_sink", &self.trace_sink.is_some())
            .finish()
    }
}

impl GeminiLiveWebSocketTransport {
    /// Creates a Gemini Live websocket transport.
    ///
    /// # Example
    ///
    /// ```
    /// let transport = lumvise_neural_core::llm_providers::gemini::live::GeminiLiveWebSocketTransport::new(None);
    /// assert!(transport.endpoint().starts_with("wss://"));
    /// ```
    pub fn new(endpoint: Option<String>) -> Self {
        Self {
            endpoint: endpoint.unwrap_or_else(|| DEFAULT_GEMINI_LIVE_ENDPOINT.to_string()),
            trace_sink: None,
        }
    }

    pub fn new_with_trace(endpoint: Option<String>, trace_sink: Arc<GeminiLiveTraceSink>) -> Self {
        Self {
            endpoint: endpoint.unwrap_or_else(|| DEFAULT_GEMINI_LIVE_ENDPOINT.to_string()),
            trace_sink: Some(trace_sink),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl GeminiLiveTransport for GeminiLiveWebSocketTransport {
    fn stream_turn(
        &self,
        turn: GeminiLiveTurn,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        let mcp_tools = GeminiLiveMcpTools::discover(&turn)?;
        let request = live_connect_request(&self.endpoint, &turn.credential)?;
        let (mut socket, _) =
            connect(request).map_err(|source| live_transport_error(&turn, source))?;
        configure_socket_timeouts(&mut socket, &turn)?;
        emit_trace(
            &self.trace_sink,
            trace_event(GeminiLiveTraceStage::WebsocketConnected),
        );
        send_json_message(&mut socket, setup_message(&turn, &mcp_tools))?;
        emit_trace(
            &self.trace_sink,
            trace_event(GeminiLiveTraceStage::SetupSent),
        );
        await_setup_complete(&mut socket, &turn, &self.trace_sink)?;
        send_turn_inputs(&mut socket, &turn, &self.trace_sink)?;
        read_live_events(
            &mut socket,
            &turn,
            control,
            &self.trace_sink,
            &mcp_tools,
            on_event,
        )
    }
}

fn configure_socket_timeouts(socket: &mut GeminiLiveSocket, turn: &GeminiLiveTurn) -> Result<()> {
    configure_realtime_socket_timeouts(
        socket,
        Some(GEMINI_LIVE_SOCKET_TIMEOUT),
        |direction, source| timeout_configuration_error(direction, turn, source),
    )
}

fn await_setup_complete(
    socket: &mut GeminiLiveSocket,
    turn: &GeminiLiveTurn,
    trace_sink: &Option<Arc<GeminiLiveTraceSink>>,
) -> Result<()> {
    loop {
        let value = read_json_message(socket, turn, trace_sink)?;
        fail_on_live_error(&value, turn)?;
        if value.get("setupComplete").is_some() {
            emit_trace(
                trace_sink,
                trace_event(GeminiLiveTraceStage::SetupCompleteReceived),
            );
            return Ok(());
        }
    }
}

fn send_turn_inputs(
    socket: &mut GeminiLiveSocket,
    turn: &GeminiLiveTurn,
    trace_sink: &Option<Arc<GeminiLiveTraceSink>>,
) -> Result<()> {
    if !turn.user_text.trim().is_empty() {
        send_json_message(socket, realtime_text_message(&turn.user_text))?;
        emit_trace(
            trace_sink,
            text_trace_event(GeminiLiveTraceStage::RealtimeTextSent, turn.user_text.len()),
        );
    }
    for input in &turn.inputs {
        send_json_message(socket, realtime_input_message(input))?;
        emit_trace(trace_sink, input_trace_event(input));
    }
    if turn.inputs.iter().any(gemini_live_input_is_audio) {
        send_json_message(
            socket,
            json!({ "realtimeInput": { "audioStreamEnd": true } }),
        )?;
        emit_trace(
            trace_sink,
            trace_event(GeminiLiveTraceStage::AudioStreamEndSent),
        );
    }
    Ok(())
}

fn read_live_events(
    socket: &mut GeminiLiveSocket,
    turn: &GeminiLiveTurn,
    control: StreamControl,
    trace_sink: &Option<Arc<GeminiLiveTraceSink>>,
    mcp_tools: &GeminiLiveMcpTools,
    on_event: &mut LlmStreamEventSink<'_>,
) -> Result<()> {
    let mut emitted = 0usize;
    loop {
        let value = read_json_message(socket, turn, trace_sink)?;
        fail_on_live_error(&value, turn)?;
        if let Some(tool_response) = mcp_tools.tool_response_for_message(turn, &value)? {
            send_json_message(socket, tool_response)?;
            continue;
        }
        for event in live_events_from_value(&value) {
            emit_stream_event_trace(trace_sink, &event);
            on_event(event)?;
            emitted += 1;
            if control.should_cancel_after(emitted) {
                emit_trace(trace_sink, trace_event(GeminiLiveTraceStage::Cancelled));
                return on_event(LlmStreamEvent::Cancelled);
            }
        }
        if live_message_is_complete(&value) {
            emit_trace(
                trace_sink,
                trace_event(GeminiLiveTraceStage::TurnCompleteReceived),
            );
            return on_event(LlmStreamEvent::Complete);
        }
    }
}

fn send_json_message(socket: &mut GeminiLiveSocket, value: Value) -> Result<()> {
    send_socket_json(
        socket,
        value,
        "gemini live websocket message",
        "serializable JSON value",
        |source| io_like_live_error("gemini live websocket send", source),
    )
}

fn read_json_message(
    socket: &mut GeminiLiveSocket,
    turn: &GeminiLiveTurn,
    trace_sink: &Option<Arc<GeminiLiveTraceSink>>,
) -> Result<Value> {
    loop {
        match read_socket_message(socket, |source| {
            let error = live_transport_error(turn, source);
            if live_error_is_timeout(&error) {
                emit_trace(
                    trace_sink,
                    trace_event(GeminiLiveTraceStage::TransportTimeout),
                );
            }
            error
        })? {
            Message::Text(text) => return parse_live_json(&text),
            Message::Binary(bytes) => return parse_live_bytes(&bytes),
            Message::Close(frame) => return Err(live_closed_error(turn, frame.as_ref())),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
        }
    }
}

fn parse_live_bytes(bytes: &[u8]) -> Result<Value> {
    let text = std::str::from_utf8(bytes).map_err(|source| NeuralError::MalformedPayload {
        value: format!("{} Gemini Live binary bytes", bytes.len()),
        expected: format!("valid UTF-8 websocket text: {source}"),
    })?;
    parse_live_json(text)
}

fn parse_live_json(text: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|source| NeuralError::Json {
        value: text.to_string(),
        expected: "Gemini Live server JSON message".to_string(),
        source,
    })
}

struct GeminiLiveMcpTools {
    tools: McpToolCatalog,
    function_declarations: Vec<Value>,
}

struct GeminiLiveFunctionCall {
    id: String,
    name: String,
    args: Value,
}

impl GeminiLiveMcpTools {
    fn discover(turn: &GeminiLiveTurn) -> Result<Self> {
        let tools = McpToolCatalog::discover(&turn.provider_id, &turn.mcp_servers)?;
        let declarations = tools.tools().iter().map(to_gemini_declaration).collect();
        Ok(Self {
            tools,
            function_declarations: declarations,
        })
    }

    fn tool_response_for_message(
        &self,
        turn: &GeminiLiveTurn,
        message: &Value,
    ) -> Result<Option<Value>> {
        let calls = gemini_live_function_calls(message)?;
        if calls.is_empty() {
            return Ok(None);
        }
        let responses = calls
            .iter()
            .map(|call| self.execute_call(turn, call))
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(
            json!({ "toolResponse": { "functionResponses": responses } }),
        ))
    }

    fn execute_call(&self, turn: &GeminiLiveTurn, call: &GeminiLiveFunctionCall) -> Result<Value> {
        let _ = turn;
        let result =
            tool_outcome_value(self.tools.invoker().invoke(&call.name, call.args.clone()))?;
        Ok(json!({
            "id": call.id,
            "name": call.name,
            "response": { "result": result }
        }))
    }
}

fn gemini_live_function_calls(message: &Value) -> Result<Vec<GeminiLiveFunctionCall>> {
    let calls = message
        .pointer("/toolCall/functionCalls")
        .or_else(|| message.pointer("/tool_call/function_calls"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    calls
        .into_iter()
        .map(gemini_live_function_call)
        .collect::<Result<Vec<_>>>()
}

fn gemini_live_function_call(value: Value) -> Result<GeminiLiveFunctionCall> {
    let name = value["name"]
        .as_str()
        .ok_or_else(|| NeuralError::InvalidValue {
            value: value.to_string(),
            expected: "Gemini Live function call with non-empty name".to_string(),
        })?;
    let id = value["id"].as_str().unwrap_or(name);
    Ok(GeminiLiveFunctionCall {
        id: id.to_string(),
        name: name.to_string(),
        args: gemini_function_call_args(&value)?,
    })
}

fn gemini_function_call_args(value: &Value) -> Result<Value> {
    let args = value
        .get("args")
        .or_else(|| value.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    if args.is_object() {
        return Ok(args);
    }
    if let Some(raw) = args.as_str() {
        return serde_json::from_str(raw).map_err(|source| NeuralError::Json {
            value: raw.to_string(),
            expected: "Gemini Live function call JSON arguments".to_string(),
            source,
        });
    }
    Err(NeuralError::InvalidValue {
        value: args.to_string(),
        expected: "Gemini Live function call args object or JSON string".to_string(),
    })
}

fn setup_message(turn: &GeminiLiveTurn, mcp_tools: &GeminiLiveMcpTools) -> Value {
    let mut message = json!({
        "setup": {
            "model": live_model_name(&turn.model),
            "generationConfig": { "responseModalities": ["TEXT"] },
            "sessionResumption": {}
        }
    });
    if let Some(handle) = turn.session_resumption_handle.as_deref() {
        message["setup"]["sessionResumption"]["handle"] = json!(handle);
    }
    if !mcp_tools.function_declarations.is_empty() {
        message["setup"]["tools"] = json!([{
            "functionDeclarations": mcp_tools.function_declarations
        }]);
    }
    message
}

fn realtime_text_message(text: &str) -> Value {
    json!({ "realtimeInput": { "text": text } })
}

fn realtime_input_message(input: &GeminiLiveInput) -> Value {
    let field_name = match input.kind {
        LlmModalityInputKind::LiveAudioChunk => "audio",
        LlmModalityInputKind::ScreenFrame | LlmModalityInputKind::ImageSnapshot => "video",
    };
    json!({
        "realtimeInput": {
            field_name: {
                "mimeType": input.media_type,
                "data": base64::engine::general_purpose::STANDARD.encode(&input.bytes)
            }
        }
    })
}

fn gemini_live_input_is_audio(input: &GeminiLiveInput) -> bool {
    input.kind == LlmModalityInputKind::LiveAudioChunk
}

fn emit_stream_event_trace(trace_sink: &Option<Arc<GeminiLiveTraceSink>>, event: &LlmStreamEvent) {
    let LlmStreamEvent::ContentDelta { text } = event else {
        return;
    };
    emit_trace(
        trace_sink,
        text_trace_event(GeminiLiveTraceStage::TextDeltaReceived, text.len()),
    );
}

fn input_trace_event(input: &GeminiLiveInput) -> GeminiLiveTraceEvent {
    let stage = match input.kind {
        LlmModalityInputKind::LiveAudioChunk => GeminiLiveTraceStage::RealtimeAudioSent,
        LlmModalityInputKind::ScreenFrame | LlmModalityInputKind::ImageSnapshot => {
            GeminiLiveTraceStage::RealtimeVideoSent
        }
    };
    GeminiLiveTraceEvent {
        stage,
        media_type: Some(input.media_type.clone()),
        bytes: Some(input.bytes.len()),
        text_chars: None,
    }
}

fn text_trace_event(stage: GeminiLiveTraceStage, text_chars: usize) -> GeminiLiveTraceEvent {
    GeminiLiveTraceEvent {
        stage,
        media_type: None,
        bytes: None,
        text_chars: Some(text_chars),
    }
}

fn trace_event(stage: GeminiLiveTraceStage) -> GeminiLiveTraceEvent {
    GeminiLiveTraceEvent {
        stage,
        media_type: None,
        bytes: None,
        text_chars: None,
    }
}

fn emit_trace(trace_sink: &Option<Arc<GeminiLiveTraceSink>>, event: GeminiLiveTraceEvent) {
    let Some(trace_sink) = trace_sink else {
        return;
    };
    trace_sink(event);
}

fn live_events_from_value(value: &Value) -> Vec<LlmStreamEvent> {
    live_session_update(value)
        .into_iter()
        .chain(
            live_text_deltas(value)
                .into_iter()
                .map(|text| LlmStreamEvent::ContentDelta { text }),
        )
        .collect()
}

fn live_session_update(value: &Value) -> Option<LlmStreamEvent> {
    let handle = value
        .pointer("/sessionResumptionUpdate/newHandle")
        .or_else(|| value.pointer("/session_resumption_update/new_handle"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(LlmStreamEvent::Session {
        provider_session_id: handle.to_string(),
    })
}

fn live_text_deltas(value: &Value) -> Vec<String> {
    let mut texts = Vec::new();
    append_model_turn_texts(value, &mut texts);
    append_transcription_text(value, "outputTranscription", &mut texts);
    append_transcription_text(value, "output_transcription", &mut texts);
    texts
}

fn append_model_turn_texts(value: &Value, texts: &mut Vec<String>) {
    let Some(parts) = value.pointer("/serverContent/modelTurn/parts") else {
        return;
    };
    for part in parts.as_array().into_iter().flatten() {
        push_non_empty_text(part.get("text"), texts);
    }
}

fn append_transcription_text(value: &Value, key: &str, texts: &mut Vec<String>) {
    push_non_empty_text(
        value
            .get("serverContent")
            .and_then(|content| content.get(key))
            .and_then(|transcription| transcription.get("text")),
        texts,
    );
}

fn push_non_empty_text(value: Option<&Value>, texts: &mut Vec<String>) {
    let Some(text) = value.and_then(Value::as_str) else {
        return;
    };
    if !text.trim().is_empty() {
        texts.push(text.to_string());
    }
}

fn live_message_is_complete(value: &Value) -> bool {
    value
        .pointer("/serverContent/turnComplete")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn fail_on_live_error(value: &Value, turn: &GeminiLiveTurn) -> Result<()> {
    let Some(error) = value.get("error") else {
        return Ok(());
    };
    Err(NeuralError::ProviderFailed {
        provider_id: turn.provider_id.clone(),
        message: format!(
            "Gemini Live returned error for model `{}`: {error}",
            turn.model
        ),
    })
}

fn live_model_name(model: &str) -> String {
    if model.starts_with("models/") {
        return model.to_string();
    }
    format!("models/{model}")
}

pub(crate) fn live_connect_request(
    endpoint: &str,
    credential: &GeminiLiveCredential,
) -> Result<Request<()>> {
    let url = live_endpoint_for_credential(endpoint, credential);
    let mut request = url
        .into_client_request()
        .map_err(|source| NeuralError::InvalidValue {
            value: endpoint.to_string(),
            expected: format!("valid Gemini Live websocket endpoint: {source}"),
        })?;
    if let GeminiLiveCredential::OAuthBearer(token) = credential {
        let header = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|source| {
            NeuralError::InvalidValue {
                value: "Gemini OAuth bearer token".to_string(),
                expected: format!("valid Authorization header value: {source}"),
            }
        })?;
        request.headers_mut().insert(AUTHORIZATION, header);
    }
    Ok(request)
}

fn live_endpoint_for_credential<'a>(
    endpoint: &'a str,
    credential: &GeminiLiveCredential,
) -> Cow<'a, str> {
    match credential {
        GeminiLiveCredential::ApiKey(key) => {
            Cow::Owned(live_endpoint_with_query(endpoint, "key", key))
        }
        GeminiLiveCredential::EphemeralToken(token) => Cow::Owned(live_endpoint_with_query(
            live_ephemeral_endpoint(endpoint),
            "access_token",
            token,
        )),
        GeminiLiveCredential::OAuthBearer(_) => Cow::Borrowed(endpoint),
    }
}

fn live_ephemeral_endpoint(endpoint: &str) -> &str {
    if endpoint == DEFAULT_GEMINI_LIVE_ENDPOINT {
        return DEFAULT_GEMINI_LIVE_CONSTRAINED_ENDPOINT;
    }
    endpoint
}

fn live_endpoint_with_query(endpoint: &str, key: &str, value: &str) -> String {
    let separator = if endpoint.contains('?') { "&" } else { "?" };
    format!("{endpoint}{separator}{key}={value}")
}

fn live_transport_error(turn: &GeminiLiveTurn, source: tungstenite::Error) -> NeuralError {
    if tungstenite_error_is_timeout(&source) {
        return live_timeout_error(turn);
    }
    NeuralError::ProviderFailed {
        provider_id: turn.provider_id.clone(),
        message: format!(
            "Gemini Live websocket transport failed for model `{}`: {source}",
            turn.model
        ),
    }
}

fn tungstenite_error_is_timeout(source: &tungstenite::Error) -> bool {
    let tungstenite::Error::Io(error) = source else {
        return false;
    };
    matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock)
}

fn live_timeout_error(turn: &GeminiLiveTurn) -> NeuralError {
    NeuralError::ProviderFailed {
        provider_id: turn.provider_id.clone(),
        message: format!(
            "Gemini Live websocket timed out after {}s for model `{}`; expected setup, streamed text, or turn completion",
            GEMINI_LIVE_SOCKET_TIMEOUT.as_secs(),
            turn.model
        ),
    }
}

fn live_error_is_timeout(error: &NeuralError) -> bool {
    matches!(
        error,
        NeuralError::ProviderFailed { message, .. } if message.contains("websocket timed out")
    )
}

fn live_closed_error(turn: &GeminiLiveTurn, frame: Option<&CloseFrame>) -> NeuralError {
    NeuralError::ProviderFailed {
        provider_id: turn.provider_id.clone(),
        message: format!(
            "Gemini Live websocket closed before turn complete for model `{}`{}",
            turn.model,
            close_frame_detail(frame)
        ),
    }
}

fn close_frame_detail(frame: Option<&CloseFrame>) -> String {
    let Some(frame) = frame else {
        return " without a close frame".to_string();
    };
    if frame.reason.is_empty() {
        return format!(" with close code `{}`", frame.code);
    }
    format!(
        " with close code `{}` and reason `{}`",
        frame.code, frame.reason
    )
}

fn timeout_configuration_error(
    direction: &str,
    turn: &GeminiLiveTurn,
    source: std::io::Error,
) -> NeuralError {
    NeuralError::Io {
        value: format!(
            "gemini live websocket {direction} timeout for model `{}`",
            turn.model
        ),
        expected: format!("timeout set to {}s", GEMINI_LIVE_SOCKET_TIMEOUT.as_secs()),
        source,
    }
}

fn io_like_live_error(value: &str, source: tungstenite::Error) -> NeuralError {
    NeuralError::Io {
        value: value.to_string(),
        expected: "successful websocket write".to_string(),
        source: std::io::Error::other(source.to_string()),
    }
}

#[cfg(test)]
mod tests;
