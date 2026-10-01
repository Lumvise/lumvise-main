//! Owns the adapter's single background connection attempt. Discovery only probes
//! readiness; application invocations retain their normal controlled deadline.

use super::AppBridgeConfig;
use lumvise_app_core::{RuntimeConnection, RuntimeCoordinatorError};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

pub(super) struct StartupConnection {
    config: AppBridgeConfig,
    connecting: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
}

impl StartupConnection {
    pub(super) fn new(config: AppBridgeConfig) -> Self {
        Self {
            config,
            connecting: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            failure: Arc::new(Mutex::new(None)),
        }
    }

    pub(super) fn probe(&self) -> Result<Option<RuntimeConnection>, String> {
        match self.config.coordinator().try_ready_connection() {
            Ok(Some(connection)) => return Ok(Some(connection)),
            Err(error) => {
                if matches!(error, RuntimeCoordinatorError::Quitting) {
                    self.config.mark_terminal();
                }
                return Err(error.to_string());
            }
            Ok(None) => {}
        }
        let failure = self
            .failure
            .lock()
            .map_err(|_| "startup failure lock poisoned")?
            .clone();
        self.start_attempt()?;
        failure.map_or(Ok(None), Err)
    }

    fn start_attempt(&self) -> Result<(), String> {
        if self.connecting.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let config = self.config.clone();
        let connecting = Arc::clone(&self.connecting);
        let cancelled = Arc::clone(&self.cancelled);
        let failure = Arc::clone(&self.failure);
        let spawned = std::thread::Builder::new()
            .name("mcp-app-connect".into())
            .spawn(move || {
                let result = config
                    .current_connection_until(Instant::now() + Duration::from_secs(60), &cancelled);
                if let Ok(mut recorded) = failure.lock() {
                    *recorded = result.err();
                }
                connecting.store(false, Ordering::Release);
            });
        if let Err(error) = spawned {
            self.connecting.store(false, Ordering::Release);
            return Err(format!(
                "app connection thread failed: {error}; expected background startup"
            ));
        }
        Ok(())
    }
}

impl Drop for StartupConnection {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}
