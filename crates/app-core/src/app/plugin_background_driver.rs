//! Owns production execution of signed compiled-plugin background exports.
//!
//! App lifecycle code may start this driver. Plugin and transport modules must
//! continue to expose typed operations rather than owning scheduler threads.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use chrono::Utc;
use lumvise_db_core::{RelationalOperation, RelationalResult, SemanticOperation, SemanticResult};
use lumvise_resource_routing::InvocationControl;

use crate::AppCore;
use tracing::{error, warn};

const DRIVER_INTERVAL: Duration = Duration::from_secs(1);
const DRIVER_SLEEP_SLICE: Duration = Duration::from_millis(50);
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(3600); // 1 hour
const CHANGE_HOOK_INTERVAL: Duration = Duration::from_secs(10);
/// Bounds one background-catalog sync inside the supervisor loop.
const CATALOG_SYNC_DEADLINE: Duration = Duration::from_secs(60);

pub(super) fn spawn(app: Arc<AppCore>, shutdown: Arc<AtomicBool>) -> JoinHandle<()> {
    thread::spawn(move || {
        thread::scope(|scope| {
            scope.spawn(|| run_supervisor(&app, &shutdown));
            scope.spawn(|| run_recurring_deliveries(&app, &shutdown));
            scope.spawn(|| run_change_hook_coordinator(&app, &shutdown));
            scope.spawn(|| run_project_refresh(&app, &shutdown));
            scope.spawn(|| run_db_maintenance(&app, &shutdown));
            // Vector reindex is delivered by run_change_hook_coordinator.
        });
    })
}

fn run_project_refresh(app: &AppCore, shutdown: &AtomicBool) {
    while !shutdown.load(Ordering::Relaxed) {
        if let Err(error) = super::project_import::refresh_local_projects(app) {
            log_driver_error("project_refresh", &error);
        }
        for _ in 0..200 {
            if shutdown.load(Ordering::Relaxed) {
                return;
            }
            thread::sleep(DRIVER_SLEEP_SLICE);
        }
    }
}

fn run_supervisor(app: &Arc<AppCore>, shutdown: &AtomicBool) {
    while !shutdown.load(Ordering::Relaxed) {
        restart_degraded_plugins(app);
        sleep_fixed_interval(shutdown);
    }
}

fn run_change_hook_coordinator(app: &AppCore, shutdown: &AtomicBool) {
    while !shutdown.load(Ordering::Relaxed) {
        if let Err(error) = app
            .plugin_endpoints()
            .run_change_hook_cycle(now_unix_seconds())
        {
            log_driver_error("change_hook_coordinator", &error.to_string());
        }
        let mut remaining = CHANGE_HOOK_INTERVAL;
        while remaining > Duration::ZERO && !shutdown.load(Ordering::Relaxed) {
            let slice = remaining.min(DRIVER_SLEEP_SLICE);
            thread::sleep(slice);
            remaining = remaining.saturating_sub(slice);
        }
    }
}

fn run_recurring_deliveries(app: &AppCore, shutdown: &AtomicBool) {
    let mut registration_generation = app.background_registration_generation().unwrap_or(0);
    let mut next_scan = next_recurring_scan_after(app, Utc::now().timestamp());
    while !shutdown.load(Ordering::Relaxed) {
        let now = Utc::now().timestamp();
        if now >= next_scan {
            if let Err(error) = app.plugin_endpoints().run_due_plugin_recurring_tasks(now) {
                log_driver_error("recurring_tasks", &error.to_string());
            }
            next_scan = next_recurring_scan_after(app, Utc::now().timestamp());
            registration_generation = app
                .background_registration_generation()
                .unwrap_or(registration_generation);
            continue;
        }
        let timeout = recurring_timeout(now, next_scan);
        match app.wait_for_background_registration_change(registration_generation, timeout) {
            Ok(true) => {
                registration_generation = app
                    .background_registration_generation()
                    .unwrap_or(registration_generation);
                next_scan = next_recurring_scan_after(app, Utc::now().timestamp());
            }
            Ok(false) => {
                if next_scan == i64::MAX {
                    continue;
                }
                if now < next_scan {
                    continue;
                }
            }
            Err(error) => {
                log_driver_error("recurring_task_registrations", &error.to_string());
                sleep_fixed_interval(shutdown);
            }
        }
    }
}

fn run_db_maintenance(app: &AppCore, shutdown: &AtomicBool) {
    let mut next_maintenance =
        Utc::now() + chrono::Duration::from_std(MAINTENANCE_INTERVAL).unwrap();
    while !shutdown.load(Ordering::Relaxed) {
        let now = Utc::now();
        if now >= next_maintenance {
            match app.semantic.execute(
                SemanticOperation::Maintenance,
                &InvocationControl::sixty_seconds(),
            ) {
                Ok(SemanticResult::MaintenanceCompleted) => {}
                Ok(result) => {
                    log_driver_error("db_maintenance", &format!("unexpected result {result:?}"))
                }
                Err(error) => log_driver_error("db_maintenance", &error.to_string()),
            }
            next_maintenance = now + chrono::Duration::from_std(MAINTENANCE_INTERVAL).unwrap();
        }
        sleep_sliced(shutdown, || false);
    }
}

fn restart_degraded_plugins(app: &Arc<AppCore>) {
    if let Err(error) = app.project_execution().expire_provider_leases() {
        log_driver_error("project_execution_leases", &error.to_string());
    }
    let plugin_ids = match app.enabled_compiled_plugin_ids() {
        Ok(plugin_ids) => plugin_ids,
        Err(error) => return log_driver_error("plugin_supervisor", &error.to_string()),
    };
    let plugin_system = app.plugin_system_handle();
    let (start_tx, start_rx) = mpsc::channel();
    let mut start_handles = Vec::new();
    for plugin_id in plugin_ids {
        if plugin_system.is_active(&plugin_id).unwrap_or(false) {
            continue;
        }
        let plugin_id_for_start = plugin_id;
        let plugin_system = Arc::clone(&plugin_system);
        let start_tx = start_tx.clone();
        start_handles.push(std::thread::spawn(move || {
            let outcome = plugin_system
                .start(&plugin_id_for_start)
                .map_err(|error| error.to_string());
            let _ = start_tx.send((plugin_id_for_start, outcome));
        }));
    }
    drop(start_tx);
    for (plugin_id, outcome) in start_rx {
        if let Err(error) = outcome {
            // A lost start race (manual restart, another supervisor tick) surfaces
            // as an error like `AlreadyActive` while the plugin is perfectly healthy.
            // The race guard skips it; every other transient start failure is logged
            // and retried on the next tick while the durable desired state stays
            // enabled. PluginSystem never publishes an inactive entry, so leaving
            // the record enabled stays fail-closed.
            if app.plugin_system().is_active(&plugin_id).unwrap_or(false) {
                log_driver_warning(
                    "plugin_supervisor_start",
                    &format!("plugin `{plugin_id}` start race resolved externally: {error}"),
                );
                continue;
            }
            log_driver_warning(
                "plugin_supervisor_start",
                &format!("plugin `{plugin_id}` start failed; enabled for retry: {error}"),
            );
            continue;
        }
        if let Err(error) = app.notify_background_registration_change() {
            log_driver_error("plugin_supervisor", &error.to_string());
            continue;
        }
        if let Err(error) = sync_background_catalog_bounded(app, now_unix_seconds()) {
            // A jammed database write gate must never stop the supervisor loop:
            // it is the only mechanism that restarts dead plugin processes.
            log_driver_error("plugin_background_catalog_sync", &error);
        } else {
            log_driver_warning("plugin_ready", &format!("plugin `{plugin_id}` started"));
        }
    }
    for handle in start_handles {
        if let Err(error) = handle.join() {
            log_driver_warning(
                "plugin_supervisor",
                &format!("thread panicked: {:?}", error),
            );
        }
    }
}

/// Runs the catalog sync off the supervisor thread with a hard deadline. The
/// sync blocks on the shared SQL write gate; without this bound one stuck
/// database write permanently disabled plugin restarts, leaving invoked-dead
/// plugins such as `builtin.canvas` unavailable until a full app restart.
fn sync_background_catalog_bounded(
    app: &Arc<AppCore>,
    now_unix_seconds: i64,
) -> Result<(), String> {
    let (sender, receiver) = mpsc::channel();
    let app = Arc::clone(app);
    thread::spawn(move || {
        let _ = sender.send(
            app.plugin_endpoints()
                .sync_background_catalog(now_unix_seconds),
        );
    });
    match receiver.recv_timeout(CATALOG_SYNC_DEADLINE) {
        Ok(Ok(_exports)) => Ok(()),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => {
            // The detached worker keeps running and finishes whenever the
            // database gate frees; the supervisor continues immediately.
            Err(format!(
                "catalog sync exceeded {}s; supervisor continues without it",
                CATALOG_SYNC_DEADLINE.as_secs_f64()
            ))
        }
    }
}

fn log_driver_warning(component: &str, message: &str) {
    warn!(
        target: "app-core::plugin_background_driver",
        component = component,
        message = %message,
        "plugin background driver warning"
    );
}

fn next_recurring_scan_after(app: &AppCore, now_unix_seconds: i64) -> i64 {
    match next_recurring_due_unix_seconds(app, now_unix_seconds) {
        Ok(Some(next_due)) if next_due > now_unix_seconds => next_due,
        Ok(None) => i64::MAX,
        Ok(_) => now_unix_seconds + 1,
        Err(error) => {
            log_driver_error("recurring_task_registrations", &error.to_string());
            now_unix_seconds + 1
        }
    }
}

fn next_recurring_due_unix_seconds(
    app: &AppCore,
    now_unix_seconds: i64,
) -> crate::Result<Option<i64>> {
    let registrations = match app.relational.execute(
        RelationalOperation::ListBackgroundRegistrations,
        &InvocationControl::sixty_seconds(),
    )? {
        RelationalResult::BackgroundRegistrations(registrations) => registrations,
        result => {
            return Err(crate::AppCoreError::unsupported(
                "background registrations",
                format!("relational result {result:?}"),
            ));
        }
    };
    let mut next_due = None;
    for registration in registrations {
        if registration.export_kind != "recurring_task" {
            continue;
        }
        let Some(candidate_due) = registration.next_due_at else {
            continue;
        };
        if candidate_due <= now_unix_seconds {
            return Ok(Some(now_unix_seconds));
        }
        match next_due {
            Some(existing_due) if candidate_due >= existing_due => {}
            _ => next_due = Some(candidate_due),
        }
    }
    Ok(next_due)
}

fn now_unix_seconds() -> i64 {
    Utc::now().timestamp()
}

fn recurring_timeout(now_unix_seconds: i64, next_scan: i64) -> Option<Duration> {
    if next_scan == i64::MAX {
        return None;
    }
    let remaining = next_scan.saturating_sub(now_unix_seconds);
    match u64::try_from(remaining) {
        Ok(remaining) => Some(Duration::from_secs(remaining)),
        Err(_) => Some(Duration::ZERO),
    }
}

fn sleep_fixed_interval(shutdown: &AtomicBool) {
    sleep_sliced(shutdown, || false);
}

fn sleep_sliced(shutdown: &AtomicBool, should_wake: impl Fn() -> bool) {
    let mut remaining = DRIVER_INTERVAL;
    while remaining > Duration::ZERO && !shutdown.load(Ordering::Relaxed) {
        let slice = remaining.min(DRIVER_SLEEP_SLICE);
        thread::sleep(slice);
        if should_wake() {
            break;
        }
        remaining = remaining.saturating_sub(slice);
    }
}

fn log_driver_error(export_kind: &str, error: &str) {
    error!(
        target: "app-core::plugin_background_driver",
        export_kind = export_kind,
        message = %error,
        "plugin background driver error"
    );
}
