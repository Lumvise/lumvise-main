use interprocess::local_socket::{
    GenericFilePath, ListenerNonblockingMode, ListenerOptions,
    prelude::{LocalSocketListener, LocalSocketStream, *},
};
use prost::Message;
use std::{
    io::{self, Read, Write},
    path::Path,
    time::Duration,
};

use lumvise_mcp_core::{decode_runtime_control_frame, frame_runtime_control};

pub(super) type ControlListener = LocalSocketListener;
pub(super) type ControlStream = LocalSocketStream;

#[cfg(target_os = "linux")]
use interprocess::os::unix::local_socket::ListenerOptionsExt;
#[cfg(windows)]
use interprocess::os::windows::{
    local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor,
};
#[cfg(windows)]
use widestring::U16CString;

pub(super) fn bind(root: &Path, generation_nonce: &str) -> io::Result<(ControlListener, String)> {
    let endpoint = endpoint(root, generation_nonce);
    let name = Path::new(&endpoint).to_fs_name::<GenericFilePath>()?;
    let options = listener_options(ListenerOptions::new().name(name))?;
    let listener = options.create_sync()?;
    restrict_endpoint(&endpoint)?;
    Ok((listener, endpoint))
}

pub(super) fn connect(endpoint: &str) -> io::Result<ControlStream> {
    let name = Path::new(endpoint).to_fs_name::<GenericFilePath>()?;
    LocalSocketStream::connect(name)
}

pub(super) fn accept(listener: &ControlListener) -> io::Result<ControlStream> {
    listener.accept()
}

pub(super) fn configure_stream(stream: &ControlStream) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_recv_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_send_timeout(Some(Duration::from_secs(2)));
}

pub(super) fn read_frame<M: Message + Default>(stream: &mut ControlStream) -> io::Result<M> {
    let mut prefix = [0_u8; 4];
    stream.read_exact(&mut prefix)?;
    let size = u32::from_be_bytes(prefix) as usize;
    let mut payload = vec![0_u8; size];
    stream.read_exact(&mut payload)?;
    let mut frame = Vec::with_capacity(prefix.len() + payload.len());
    frame.extend_from_slice(&prefix);
    frame.extend_from_slice(&payload);
    decode_runtime_control_frame(&frame).map_err(io::Error::other)
}

pub(super) fn write_frame<M: Message>(stream: &mut ControlStream, message: &M) -> io::Result<()> {
    stream.write_all(&frame_runtime_control(message))?;
    stream.flush()
}

#[cfg(unix)]
fn endpoint(root: &Path, _generation_nonce: &str) -> String {
    root.join("control.sock").display().to_string()
}

#[cfg(windows)]
fn endpoint(_root: &Path, generation_nonce: &str) -> String {
    format!(r"\\.\pipe\lumvise-control-{generation_nonce}")
}

#[cfg(all(test, windows))]
mod tests {
    use super::endpoint;
    use interprocess::local_socket::{GenericFilePath, ToFsName};
    use std::path::Path;

    #[test]
    fn windows_endpoint_is_generation_unique_named_pipe() {
        let first = endpoint(Path::new(r"C:\runtime"), "generation-a");
        let second = endpoint(Path::new(r"C:\runtime"), "generation-b");

        assert!(first.starts_with(r"\\.\pipe\"));
        assert!(second.starts_with(r"\\.\pipe\"));
        assert_ne!(first, second);
        Path::new(&first)
            .to_fs_name::<GenericFilePath>()
            .expect("named pipe endpoint should be accepted");
    }
}

#[cfg(unix)]
pub(super) fn remove_endpoint(endpoint: &str) {
    let _ = std::fs::remove_file(endpoint);
}

#[cfg(windows)]
pub(super) fn remove_endpoint(_endpoint: &str) {}

#[cfg(unix)]
fn restrict_endpoint(endpoint: &str) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600))
}

#[cfg(windows)]
fn restrict_endpoint(_endpoint: &str) -> io::Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn listener_options(options: ListenerOptions<'_>) -> io::Result<ListenerOptions<'_>> {
    Ok(options
        .nonblocking(ListenerNonblockingMode::Accept)
        .try_overwrite(true)
        .max_spin_time(Duration::from_millis(100))
        .mode(0o600))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn listener_options(options: ListenerOptions<'_>) -> io::Result<ListenerOptions<'_>> {
    Ok(options
        .nonblocking(ListenerNonblockingMode::Accept)
        .try_overwrite(true)
        .max_spin_time(Duration::from_millis(100)))
}

#[cfg(windows)]
fn listener_options(options: ListenerOptions<'_>) -> io::Result<ListenerOptions<'_>> {
    let sddl = U16CString::from_str("D:P(A;;GA;;;OW)(A;;GA;;;SY)")
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    let descriptor = SecurityDescriptor::deserialize(&sddl)?;
    Ok(options
        .nonblocking(ListenerNonblockingMode::Accept)
        .security_descriptor(descriptor))
}
