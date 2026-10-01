//! Production connection construction. Credentials never cross into renderer state.
use super::audio::{AudioSessionEventSink, AudioSessionInput, AudioSessionRequest, invalid_audio};
use super::audio_driver::AudioSessionDriver;
use super::audio_wire::{AudioSocket, AudioWireDialect};
use super::{RealtimeSocket, configure_socket_timeouts};
use crate::config::LlmProviderConfig;
use crate::error::Result;
use crate::llm_providers::tool_invocation::{
    McpToolCatalog,
    schema_adapters::{to_gemini_declaration, to_openai_function_tool},
};
use serde_json::Value;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tungstenite::Message;

const AUDIO_INSTRUCTIONS: &str = "This is a direct audio conversation. Speak using your native audio output. Do not call assistant.respond to synthesize speech. Use the available MCP functions for canvas diagrams, pictures, project knowledge, and background questions. The application records your audio transcript. Keep explanations short and natural; finish only when the user asks.";

pub(crate) fn run_openai(
    config: &LlmProviderConfig,
    request: AudioSessionRequest,
    input: AudioSessionInput,
    sink: &mut AudioSessionEventSink<'_>,
) -> Result<()> {
    let turn =
        crate::llm_providers::codex::live::openai_realtime_turn(config, &request.conversation)?;
    let mut connection = crate::llm_providers::codex::live::openai_realtime_connect_request(&turn)?;
    connection.headers_mut().remove("openai-beta");
    let tools = Arc::new(McpToolCatalog::discover(
        &turn.provider_id,
        &turn.mcp_servers,
    )?);
    let wire = super::openai_audio::OpenAiAudioWire {
        model: turn.model,
        voice: request.voice.unwrap_or_else(|| "marin".into()),
        instructions: audio_instructions(&request.conversation),
        tools: tools
            .tools()
            .iter()
            .filter(|tool| audio_tool(&tool.name))
            .map(to_openai_function_tool)
            .collect(),
        response_id: String::new(),
        response_active: false,
    };
    run(connection, Box::new(wire), tools, input, sink)
}

pub(crate) fn run_gemini(
    config: &LlmProviderConfig,
    request: AudioSessionRequest,
    input: AudioSessionInput,
    sink: &mut AudioSessionEventSink<'_>,
) -> Result<()> {
    let turn = crate::llm_providers::gemini::gemini_live_turn(config, &request.conversation)?;
    let endpoint = config
        .endpoint
        .as_deref()
        .filter(|url| url.starts_with("ws"))
        .unwrap_or(crate::llm_providers::gemini::live::DEFAULT_GEMINI_LIVE_ENDPOINT);
    let connection =
        crate::llm_providers::gemini::live::live_connect_request(endpoint, &turn.credential)?;
    let tools = Arc::new(McpToolCatalog::discover(
        &turn.provider_id,
        &turn.mcp_servers,
    )?);
    let wire = super::gemini_audio::GeminiAudioWire {
        model: turn.model,
        voice: request.voice.unwrap_or_else(|| "Aoede".into()),
        instructions: audio_instructions(&request.conversation),
        tools: tools
            .tools()
            .iter()
            .filter(|tool| audio_tool(&tool.name))
            .map(to_gemini_declaration)
            .collect(),
        turn: 0,
        response_active: false,
        input_transcript: String::new(),
        suppress_response: false,
        resumption_handle: None,
        reconnect_requested: false,
    };
    run(connection, Box::new(wire), tools, input, sink)
}

fn run(
    connection: tungstenite::http::Request<()>,
    wire: Box<dyn AudioWireDialect>,
    tools: Arc<McpToolCatalog>,
    input: AudioSessionInput,
    sink: &mut AudioSessionEventSink<'_>,
) -> Result<()> {
    let socket = connect_audio_socket(connection.clone())?;
    AudioSessionDriver::new(
        Box::new(ProviderAudioSocket { socket, connection }),
        wire,
        tools,
    )
    .run(input, sink)
}

fn connect_audio_socket(connection: tungstenite::http::Request<()>) -> Result<RealtimeSocket> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let host = connection
        .uri()
        .host()
        .ok_or_else(|| invalid_audio(connection.uri(), "WebSocket host"))?;
    let port =
        connection
            .uri()
            .port_u16()
            .unwrap_or(if connection.uri().scheme_str() == Some("wss") {
                443
            } else {
                80
            });
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| invalid_audio(error, "resolvable audio host"))?;
    let mut connected = None;
    for address in addresses {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        if let Ok(stream) = TcpStream::connect_timeout(&address, remaining) {
            connected = Some(stream);
            break;
        }
    }
    let stream =
        connected.ok_or_else(|| invalid_audio(host, "reachable audio host within 60 seconds"))?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    stream
        .set_read_timeout(Some(remaining))
        .and_then(|_| stream.set_write_timeout(Some(remaining)))
        .map_err(|error| invalid_audio(error, "audio handshake deadline"))?;
    let (mut socket, _) = tungstenite::client_tls_with_config(connection, stream, None, None)
        .map_err(|error| invalid_audio(error, "connected audio WebSocket"))?;
    configure_socket_timeouts(&mut socket, Some(Duration::from_secs(60)), |_, error| {
        invalid_audio(error, "audio write deadline")
    })?;
    set_audio_read_poll(&mut socket)?;
    Ok(socket)
}

fn set_audio_read_poll(socket: &mut RealtimeSocket) -> Result<()> {
    use tungstenite::stream::MaybeTlsStream;
    let stream = match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(stream) => &mut stream.sock,
        _ => {
            return Err(invalid_audio(
                "TLS backend",
                "plain TCP or Rustls audio stream",
            ));
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_millis(10)))
        .map_err(|error| invalid_audio(error, "audio input polling timeout"))
}

fn audio_instructions(request: &crate::llm_providers::LlmRequest) -> String {
    let context = request
        .messages
        .iter()
        .filter(|message| message.role == "system")
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    format!("{context}\n\n{AUDIO_INSTRUCTIONS}")
}

pub(super) fn audio_tool(name: &str) -> bool {
    ![
        "assistant.respond",
        "assistant_respond",
        "assistant.await_turn",
        "assistant_await_turn",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix))
}

struct ProviderAudioSocket {
    socket: RealtimeSocket,
    connection: tungstenite::http::Request<()>,
}

impl AudioSocket for ProviderAudioSocket {
    fn send(&mut self, message: Value) -> Result<()> {
        self.socket
            .send(Message::Text(message.to_string().into()))
            .map_err(|error| invalid_audio(error, "audio socket send"))
    }

    fn receive(&mut self) -> Result<Option<Value>> {
        match self.socket.read() {
            Ok(Message::Text(text)) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|_| invalid_audio(text, "audio provider JSON event")),
            Ok(Message::Close(_)) => Err(invalid_audio(
                "socket closed",
                "connected audio conversation; reconnect required",
            )),
            Ok(_) => Ok(None),
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(invalid_audio(error, "audio socket event")),
        }
    }

    fn reconnect(&mut self) -> Result<()> {
        self.close();
        self.socket = connect_audio_socket(self.connection.clone())?;
        Ok(())
    }

    fn close(&mut self) {
        let _ = self.socket.close(None);
    }
}
