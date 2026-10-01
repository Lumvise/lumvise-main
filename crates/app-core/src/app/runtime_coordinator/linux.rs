#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};

pub(crate) fn lease_path(root: &Path) -> PathBuf {
    root.join("owner.lock")
}

pub(crate) fn launch_desktop(request: &super::ActivationRequest) -> std::io::Result<()> {
    let mut dbus = std::process::Command::new("dbus-send");
    dbus.args([
        "--session",
        "--type=method_call",
        "--dest=org.lumvise.App",
        "/org/lumvise/App",
        "org.freedesktop.Application.Activate",
    ]);
    if dbus.status().is_ok_and(|status| status.success()) {
        return Ok(());
    }
    let executable = std::env::var_os("LUMVISE_EXECUTABLE").unwrap_or_else(|| "lumvise".into());
    std::process::Command::new(executable)
        .args(&request.arguments)
        .spawn()
        .map(|_| ())
}
