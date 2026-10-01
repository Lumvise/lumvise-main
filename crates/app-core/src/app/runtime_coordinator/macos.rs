#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};

pub(crate) fn lease_path(root: &Path) -> PathBuf {
    root.join("owner.lock")
}

pub(crate) fn launch_desktop(request: &super::ActivationRequest) -> std::io::Result<()> {
    std::process::Command::new("open")
        .arg("-a")
        .arg("Lumvise")
        .arg("--args")
        .args(&request.arguments)
        .spawn()
        .map(|_| ())
}
