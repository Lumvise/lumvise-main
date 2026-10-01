pub mod audio;
pub(crate) mod audio_connection;
mod audio_driver;
#[cfg(test)]
mod audio_tests;
mod audio_wire;
pub(crate) mod events;
mod gemini_audio;
mod openai_audio;
mod socket;

pub(crate) use socket::{
    RealtimeSocket, configure_socket_timeouts, read_socket_message, send_socket_json,
};
