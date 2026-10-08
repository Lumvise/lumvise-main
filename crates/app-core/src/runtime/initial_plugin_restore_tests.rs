use super::AppCore;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

struct PendingPluginCatalog {
    completion: Arc<(Mutex<bool>, Condvar)>,
}

impl PendingPluginCatalog {
    fn new(app: &mut AppCore) -> Self {
        let completion = Arc::new((Mutex::new(false), Condvar::new()));
        app.initial_plugin_restore = Some(Arc::clone(&completion));
        Self { completion }
    }

    fn finish(&self) {
        let (complete, notifier) = self.completion.as_ref();
        *complete.lock().unwrap() = true;
        notifier.notify_all();
    }
}

#[test]
fn initial_plugin_restore_wait_reports_timeout_then_completion() {
    let mut app = AppCore::in_memory().unwrap();
    let catalog = PendingPluginCatalog::new(&mut app);
    assert!(
        !app.wait_for_initial_plugin_restore(Some(Duration::from_millis(1)))
            .unwrap()
    );

    catalog.finish();
    assert!(
        app.wait_for_initial_plugin_restore(Some(Duration::from_secs(1)))
            .unwrap()
    );
}

#[test]
fn initial_plugin_restore_without_batch_deadline_waits_for_catalog_completion() {
    let mut app = AppCore::in_memory().unwrap();
    let catalog = PendingPluginCatalog::new(&mut app);
    let (result_sender, result_receiver) = mpsc::channel();
    let restore_wait = std::thread::spawn(move || {
        result_sender
            .send(app.wait_for_initial_plugin_restore(None))
            .unwrap();
    });
    assert!(matches!(
        result_receiver.recv_timeout(Duration::from_millis(20)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    catalog.finish();
    assert!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap()
    );
    restore_wait.join().unwrap();
}

#[test]
fn initial_plugin_restore_without_production_catalog_is_immediately_ready() {
    let app = AppCore::in_memory().unwrap();
    assert!(app.wait_for_initial_plugin_restore(None).unwrap());
    assert!(
        app.wait_for_initial_plugin_restore(Some(Duration::ZERO))
            .unwrap()
    );
}
