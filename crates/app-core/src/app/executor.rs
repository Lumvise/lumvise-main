use crate::app::desktop::PendingDesktopBridge;
use crate::{
    AppCore, AppCoreDesktopBridge, AppResourceSelection, OwnerLease, QuitRequest,
    RuntimeControlPort,
};
use lumvise_frontend_core::{
    AppSettings, DesktopAppConfig, DesktopLifecyclePort, PendingDesktopLifecyclePort,
};
use std::sync::{Arc, Mutex};
use tracing::{error, info};

struct DesktopRuntimeControlPort {
    lifecycle: Arc<dyn DesktopLifecyclePort>,
}

impl RuntimeControlPort for DesktopRuntimeControlPort {
    fn activate(&self, _arguments: Vec<String>) -> Result<(), String> {
        self.lifecycle.activate_main_window()
    }

    fn quit(&self) -> Result<(), String> {
        self.lifecycle.request_quit()
    }
}

/// Starts the normal connected Lumvise desktop app.
///
/// # Example
///
/// ```ignore
/// lumvise_app_core::run_lumvise_app(owner, launch_desktop).unwrap();
/// ```
pub fn run_lumvise_app(
    owner: OwnerLease,
    launch: impl FnOnce(DesktopAppConfig) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    run_lumvise_app_with_resource_selection(owner, || Ok(AppResourceSelection::Environment), launch)
}

/// Selects resources once on the startup thread while recovery Settings remain available.
/// Example: `run_lumvise_app_with_resource_selection(owner, load_connection, launch)`.
/// A failed selection publishes startup failure; it never falls back to a local store.
pub fn run_lumvise_app_with_resource_selection(
    owner: OwnerLease,
    select: impl FnOnce() -> Result<AppResourceSelection, String> + Send + 'static,
    launch: impl FnOnce(DesktopAppConfig) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    crate::observability::init();
    let startup_bridge = Arc::new(PendingDesktopBridge::new());
    let lifecycle_port = Arc::new(PendingDesktopLifecyclePort::default());
    let owner_cell = Arc::new(Mutex::new(Some(owner)));
    let quit_owner = Arc::clone(&owner_cell);
    lifecycle_port.set_quit_hook(Arc::new(move || {
        let mut owner = quit_owner
            .lock()
            .map_err(|_| "runtime owner lock poisoned".to_string())?
            .take();
        if let Some(mut owner) = owner.take() {
            owner
                .begin_quit(QuitRequest::default())
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }))?;
    owner_cell
        .lock()
        .map_err(|_| "runtime owner lock poisoned".to_string())?
        .as_ref()
        .ok_or_else(|| "runtime owner missing before control install".to_string())?
        .install_control_port(Arc::new(DesktopRuntimeControlPort {
            lifecycle: lifecycle_port.clone(),
        }))?;
    let config = DesktopAppConfig {
        settings: AppSettings::default(),
        settings_bridge: startup_bridge.clone(),
        semantic_graph_bridge: startup_bridge.clone(),
        whiteboard_bridge: startup_bridge.clone(),
        voice_bridge: startup_bridge.clone(),
        renderer_tool: std::env::var("LUMVISE_RENDERER_TOOL").ok(),
        lifecycle_port,
        ..Default::default()
    };

    let startup_bridge = Arc::clone(&startup_bridge);
    let _startup_thread = std::thread::Builder::new()
        .name("lumvise-desktop-startup".to_string())
        .spawn(move || {
            if let Err(error) = select().and_then(|selection| initialize_desktop_app(startup_bridge.clone(), owner_cell, selection)) {
                let message = format!("desktop startup failed: {error}");
                error!(target: "app-core::desktop", event = "startup_failed", error = %error, "{message}");
                if let Err(action_error) = startup_bridge
                    .record_startup_phase("failed", Some(&message))
                {
                    error!(target: "app-core::desktop", event = "startup_failed_action_failed", error = %action_error, "failed to persist startup phase status");
                }
            }
        })
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;

    launch(config)?;
    Ok(())
}
fn initialize_desktop_app(
    startup_bridge: Arc<PendingDesktopBridge>,
    owner_cell: Arc<Mutex<Option<OwnerLease>>>,
    selection: AppResourceSelection,
) -> Result<(), String> {
    startup_bridge
        .record_startup_phase("resource_routing", Some("selecting capability resources"))?;
    let app = super::startup::build_app_runtime_with_selection(selection)?;
    startup_bridge
        .record_startup_phase("plugins_ready", Some("plugins and runtime initialized"))?;
    let delegate = Arc::new(AppCoreDesktopBridge::new_with_owner_cell(
        Arc::clone(&app),
        owner_cell,
    )?);
    startup_bridge
        .install(delegate)
        .map_err(|error| format!("installing startup bridge delegate: {error}"))?;
    startup_bridge.record_startup_phase("ready", Some("startup complete"))?;
    info!(target: "app-core::desktop", event = "startup_ready", "desktop startup ready");
    spawn_voice_warmup(app, startup_bridge);
    Ok(())
}

/// Warms deferred voice engines behind the live UI; reports readiness as a phase event.
fn spawn_voice_warmup(app: Arc<AppCore>, startup_bridge: Arc<PendingDesktopBridge>) {
    let stt = app.voice2text_service();
    let tts = app.text2voice_service();
    if stt.is_none() && tts.is_none() {
        let _ = startup_bridge
            .record_startup_phase("voice_ready", Some("voice services not configured"));
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("lumvise-voice-warmup".to_string())
        .spawn(move || {
            let mut failures = Vec::new();
            if let Some(stt) = stt
                && let Err(error) = stt.warmup()
            {
                failures.push(format!("stt: {error}"));
            }
            if let Some(tts) = tts
                && let Err(error) = tts.warmup()
            {
                failures.push(format!("tts: {error}"));
            }
            let report = if failures.is_empty() {
                startup_bridge.record_startup_phase("voice_ready", Some("voice services ready"))
            } else {
                let message = failures.join("; ");
                error!(target: "app-core::desktop", event = "voice_warmup_failed", error = %message, "voice warmup failed");
                startup_bridge.record_startup_phase("voice_warmup_failed", Some(&message))
            };
            if let Err(error) = report {
                error!(target: "app-core::desktop", event = "voice_phase_report_failed", error = %error, "failed to report voice phase");
            }
        });
    if let Err(error) = spawned {
        error!(target: "app-core::desktop", event = "voice_warmup_spawn_failed", error = %error, "failed to spawn voice warmup thread");
    }
}

#[cfg(test)]
mod launch_tests {
    use super::*;
    use crate::{AcquireResult, ActivationRequest, AppRuntimeCoordinator};

    struct RejectingDesktopLaunch;

    impl RejectingDesktopLaunch {
        fn launch(config: DesktopAppConfig) -> Result<(), Box<dyn std::error::Error>> {
            assert_eq!(config.title, "Lumvise");
            assert!(config.show_in_taskbar);
            assert_eq!(
                Arc::as_ptr(&config.settings_bridge) as *const (),
                Arc::as_ptr(&config.voice_bridge) as *const ()
            );
            assert_eq!(
                Arc::as_ptr(&config.settings_bridge) as *const (),
                Arc::as_ptr(&config.semantic_graph_bridge) as *const ()
            );
            assert_eq!(
                Arc::as_ptr(&config.settings_bridge) as *const (),
                Arc::as_ptr(&config.whiteboard_bridge) as *const ()
            );
            config.lifecycle_port.request_quit()?;
            Err(std::io::Error::other("fake desktop launch failed").into())
        }
    }

    fn assert_isolated_desktop_launch() {
        let root = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env_clear()
            .args(["--exact", "app::executor::launch_tests::injected_desktop_launch_receives_bridges_and_ordered_quit_hook", "--nocapture"])
            .env("LUMVISE_DESKTOP_LAUNCH_TEST_CHILD", "1")
            .env("LUMVISE_RUNTIME_ROOT", root.path().join("runtime"))
            .env("LUMVISE_DB_PATH", root.path().join("database"))
            .env("LUMVISE_PLUGIN_ROOT", root.path().join("plugins"))
            .env("LUMVISE_STATE_ROOT", root.path().join("state"))
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn assert_launch_failure_releases_owner() {
        let coordinator = AppRuntimeCoordinator::production();
        let AcquireResult::Owner(owner) = coordinator
            .acquire_or_forward(ActivationRequest::default())
            .unwrap()
        else {
            panic!("isolated runtime must acquire ownership")
        };
        let error = run_lumvise_app(owner, RejectingDesktopLaunch::launch).unwrap_err();
        assert_eq!(error.to_string(), "fake desktop launch failed");
        let replacement = coordinator
            .acquire_or_forward(ActivationRequest::default())
            .unwrap();
        assert!(matches!(replacement, AcquireResult::Owner(_)));
    }

    #[test]
    fn injected_desktop_launch_receives_bridges_and_ordered_quit_hook() {
        if std::env::var_os("LUMVISE_DESKTOP_LAUNCH_TEST_CHILD").is_none() {
            assert_isolated_desktop_launch();
            return;
        }
        assert_launch_failure_releases_owner();
    }
}
