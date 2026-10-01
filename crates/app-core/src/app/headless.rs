//! Community runtime lifecycle; windowing and commercial plugins are optional.
//! `run_headless_app` shares production startup, readiness and shutdown with desktop.

use super::{runtime_bridge::start_runtime_bridge, startup::build_app_runtime};
use crate::{OwnerLease, QuitRequest, RuntimeControlPort};
use std::sync::Arc;
use tokio::sync::watch;

struct HeadlessRuntimeControl {
    shutdown: watch::Sender<bool>,
}

impl RuntimeControlPort for HeadlessRuntimeControl {
    fn activate(&self, _arguments: Vec<String>) -> Result<(), String> {
        Ok(())
    }

    fn quit(&self) -> Result<(), String> {
        self.shutdown
            .send(true)
            .map_err(|error| format!("requesting headless shutdown: {error}"))
    }
}

/// Runs the production plugin host without a desktop or bundled commercial plugins.
///
/// # Example
/// ```ignore
/// lumvise_app_core::run_headless_app(owner)?;
/// ```
pub fn run_headless_app(mut owner: OwnerLease) -> Result<(), Box<dyn std::error::Error>> {
    crate::observability::init();
    let (shutdown, mut requested) = watch::channel(false);
    owner.install_control_port(Arc::new(HeadlessRuntimeControl { shutdown }))?;
    let app = build_app_runtime()?;
    let (_server, _connection) = start_runtime_bridge(&app, &mut owner)?;
    tracing::info!(target: "app-core::headless", event = "startup_ready", "community runtime ready");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(wait_for_shutdown(&mut requested));
    owner.begin_quit(QuitRequest::default())?;
    result.map_err(Into::into)
}

async fn wait_for_shutdown(requested: &mut watch::Receiver<bool>) -> std::io::Result<()> {
    tokio::select! {
        _ = requested.wait_for(|value| *value) => Ok(()),
        result = tokio::signal::ctrl_c() => result,
        result = termination_signal() => result,
    }
}

#[cfg(unix)]
async fn termination_signal() -> std::io::Result<()> {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?
        .recv()
        .await;
    Ok(())
}

#[cfg(not(unix))]
async fn termination_signal() -> std::io::Result<()> {
    std::future::pending().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn authenticated_quit_requested_before_wait_is_retained() {
        let (shutdown, mut requested) = watch::channel(false);
        let control = HeadlessRuntimeControl { shutdown };
        control.activate(vec!["project".into()]).unwrap();
        control.quit().unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            wait_for_shutdown(&mut requested),
        )
        .await
        .unwrap()
        .unwrap();
    }
}
