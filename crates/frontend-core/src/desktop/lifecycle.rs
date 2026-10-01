use std::sync::{Arc, Mutex};

/// Frontend-owned operations requested by the App Runtime coordinator.
pub trait DesktopLifecyclePort: Send + Sync {
    /// Shows and focuses the main window after an activation request.
    fn activate_main_window(&self) -> Result<(), String>;
    /// Starts the coordinator-owned ordered quit sequence.
    fn request_quit(&self) -> Result<(), String>;
}

enum PendingLifecycleEvent {
    Activate,
    Quit,
}

#[derive(Default)]
struct PendingLifecycleState {
    delegate: Option<Arc<dyn DesktopLifecyclePort>>,
    queued: Vec<PendingLifecycleEvent>,
    quit_hook: Option<Arc<dyn Fn() -> Result<(), String> + Send + Sync>>,
}

/// Lifecycle port available before Tauri finishes creating its AppHandle.
#[derive(Default)]
pub struct PendingDesktopLifecyclePort {
    state: Mutex<PendingLifecycleState>,
}

impl PendingDesktopLifecyclePort {
    /// Installs a coordinator-owned ordered quit hook used by tray and control requests.
    pub fn set_quit_hook(
        &self,
        hook: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    ) -> Result<(), String> {
        self.state
            .lock()
            .map_err(|_| "pending desktop lifecycle lock poisoned".to_string())?
            .quit_hook = Some(hook);
        Ok(())
    }

    /// Installs the native delegate and replays startup-time lifecycle requests.
    pub fn install(&self, delegate: Arc<dyn DesktopLifecyclePort>) -> Result<(), String> {
        let queued = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "pending desktop lifecycle lock poisoned".to_string())?;
            state.delegate = Some(Arc::clone(&delegate));
            std::mem::take(&mut state.queued)
        };
        for event in queued {
            match event {
                PendingLifecycleEvent::Activate => delegate.activate_main_window()?,
                PendingLifecycleEvent::Quit => delegate.request_quit()?,
            }
        }
        Ok(())
    }

    fn delegate_or_queue(
        &self,
        event: PendingLifecycleEvent,
    ) -> Result<Option<Arc<dyn DesktopLifecyclePort>>, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "pending desktop lifecycle lock poisoned".to_string())?;
        if let Some(delegate) = state.delegate.as_ref() {
            return Ok(Some(Arc::clone(delegate)));
        }
        state.queued.push(event);
        Ok(None)
    }
}

impl DesktopLifecyclePort for PendingDesktopLifecyclePort {
    fn activate_main_window(&self) -> Result<(), String> {
        if let Some(delegate) = self.delegate_or_queue(PendingLifecycleEvent::Activate)? {
            delegate.activate_main_window()?;
        }
        Ok(())
    }

    fn request_quit(&self) -> Result<(), String> {
        let quit_hook = self
            .state
            .lock()
            .map_err(|_| "pending desktop lifecycle lock poisoned".to_string())?
            .quit_hook
            .clone();
        if let Some(quit_hook) = quit_hook {
            quit_hook()?;
        }
        if let Some(delegate) = self.delegate_or_queue(PendingLifecycleEvent::Quit)? {
            delegate.request_quit()?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for PendingDesktopLifecyclePort {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingDesktopLifecyclePort")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;
    use std::sync::Mutex as TestMutex;

    struct RecordingLifecyclePort {
        events: Arc<TestMutex<Vec<&'static str>>>,
    }

    impl DesktopLifecyclePort for RecordingLifecyclePort {
        fn activate_main_window(&self) -> Result<(), String> {
            if let Ok(mut events) = self.events.lock() {
                events.push("activate");
            }
            Ok(())
        }

        fn request_quit(&self) -> Result<(), String> {
            if let Ok(mut events) = self.events.lock() {
                events.push("quit");
            }
            Ok(())
        }
    }

    #[test]
    fn pending_lifecycle_replays_requests_in_arrival_order() {
        let pending = PendingDesktopLifecyclePort::default();
        pending.activate_main_window().unwrap();
        pending.request_quit().unwrap();
        pending.activate_main_window().unwrap();

        let events = Arc::new(TestMutex::new(Vec::new()));
        pending
            .install(Arc::new(RecordingLifecyclePort {
                events: Arc::clone(&events),
            }))
            .unwrap();

        let recorded = events
            .lock()
            .ok()
            .map(|events| events.clone())
            .unwrap_or_default();
        assert_eq!(recorded, vec!["activate", "quit", "activate"]);
    }

    #[test]
    fn quit_hook_runs_before_native_delegate() {
        let pending = PendingDesktopLifecyclePort::default();
        let hook_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook_state = Arc::clone(&hook_called);
        pending
            .set_quit_hook(Arc::new(move || {
                hook_state.store(true, std::sync::atomic::Ordering::Release);
                Ok(())
            }))
            .unwrap();
        pending.request_quit().unwrap();
        assert!(hook_called.load(std::sync::atomic::Ordering::Acquire));
    }
}
