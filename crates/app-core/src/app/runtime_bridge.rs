//! Owns authenticated bridge publication for both runtime frontends.
//! Call `start_runtime_bridge` only after composing the production App Core.

use crate::{AppCore, OwnerLease, RuntimeConnection, ScopedMcpHttpServer};
use std::sync::Arc;
use std::time::Duration;

pub(super) fn start_runtime_bridge(
    app: &Arc<AppCore>,
    owner: &mut OwnerLease,
) -> Result<(ScopedMcpHttpServer, RuntimeConnection), String> {
    let ready = app
        .wait_for_initial_plugin_restore(Duration::from_secs(60))
        .map_err(|error| format!("waiting for initial plugin restore: {error}"))?;
    if !ready {
        return Err(
            "initial plugin restore exceeded 60000ms; expected ready plugin catalog".into(),
        );
    }
    let server = ScopedMcpHttpServer::spawn(Arc::clone(app))
        .map_err(|error| format!("starting App Bridge: {error}"))?;
    install_bridge_credentials(app, owner)?;
    let connection = owner
        .mark_ready(server.base_url().to_string())
        .map_err(|error| format!("publishing Runtime readiness: {error}"))?;
    Ok((server, connection))
}

fn install_bridge_credentials(app: &AppCore, owner: &mut OwnerLease) -> Result<(), String> {
    let (credential, expires_unix_ms) = owner
        .prepare_bridge_credential()
        .map_err(|error| format!("preparing App Bridge credential: {error}"))?;
    app.install_bridge_credential_store(owner.bridge_credential_store())
        .map_err(|error| format!("installing App Bridge credential store: {error}"))?;
    app.set_bridge_credential(Some(credential), expires_unix_ms)
        .map_err(|error| format!("installing App Bridge credential: {error}"))
}
