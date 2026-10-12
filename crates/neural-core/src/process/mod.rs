pub mod failure_diagnostics;
pub mod protocol;
pub mod spawned_worker;
pub mod stream;

pub use failure_diagnostics::ProcessFailureDiagnostics;
pub use protocol::{
    SPAWNED_ENGINE_PROTOCOL_MAJOR, SpawnedChatMessage, SpawnedEnvelope, SpawnedEnvelopeKind,
    SpawnedMcpServer, SpawnedOperation,
};
pub use spawned_worker::SpawnedWorker;
pub use stream::StreamControl;

/// Keeps Windows from opening a console window for a spawned helper process.
pub(crate) fn hide_console_window(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}
