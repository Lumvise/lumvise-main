use crate::error::{NeuralError, Result};
use serde_json::Value;
use std::time::Duration;
use tungstenite::protocol::Message;
use tungstenite::stream::MaybeTlsStream;

pub(crate) type RealtimeSocket = tungstenite::WebSocket<MaybeTlsStream<std::net::TcpStream>>;

pub(crate) fn configure_socket_timeouts(
    socket: &mut RealtimeSocket,
    timeout: Option<Duration>,
    error_for: impl Fn(&'static str, std::io::Error) -> NeuralError,
) -> Result<()> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => set_tcp_timeouts(stream, timeout, error_for),
        MaybeTlsStream::Rustls(stream) => set_tcp_timeouts(&stream.sock, timeout, error_for),
        _ => Ok(()),
    }
}

pub(crate) fn send_socket_json(
    socket: &mut RealtimeSocket,
    value: Value,
    value_name: &str,
    expected_json: &str,
    error_for: impl Fn(tungstenite::Error) -> NeuralError,
) -> Result<()> {
    let text = serde_json::to_string(&value).map_err(|source| NeuralError::Json {
        value: value_name.to_string(),
        expected: expected_json.to_string(),
        source,
    })?;
    socket.send(Message::Text(text.into())).map_err(error_for)
}

pub(crate) fn read_socket_message(
    socket: &mut RealtimeSocket,
    error_for: impl Fn(tungstenite::Error) -> NeuralError,
) -> Result<Message> {
    loop {
        match socket.read().map_err(&error_for)? {
            message @ (Message::Text(_) | Message::Binary(_) | Message::Close(_)) => {
                return Ok(message);
            }
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
        }
    }
}

fn set_tcp_timeouts(
    stream: &std::net::TcpStream,
    timeout: Option<Duration>,
    error_for: impl Fn(&'static str, std::io::Error) -> NeuralError,
) -> Result<()> {
    stream
        .set_read_timeout(timeout)
        .map_err(|source| error_for("read", source))?;
    stream
        .set_write_timeout(timeout)
        .map_err(|source| error_for("write", source))
}
