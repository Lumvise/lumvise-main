use lumvise_app_core::{
    AcquireResult, ActivationRequest, AppRuntimeCoordinator, QuitRequest, RuntimeControlPort,
    RuntimeCoordinatorError,
};
use parking_lot::Mutex;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Default)]
struct RecordingControlPort {
    activations: Mutex<Vec<Vec<String>>>,
}

impl RuntimeControlPort for RecordingControlPort {
    fn activate(&self, arguments: Vec<String>) -> Result<(), String> {
        self.activations.lock().push(arguments);
        Ok(())
    }

    fn quit(&self) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn runtime_discovery_is_generation_bound_and_atomic() {
    let workspace = tempfile::tempdir().expect("runtime workspace");
    let coordinator = AppRuntimeCoordinator::new(workspace.path(), |_| Ok(()));
    let owner = coordinator
        .acquire_or_forward(ActivationRequest::default())
        .expect("first owner");
    let mut lease = match owner {
        AcquireResult::Owner(lease) => lease,
        AcquireResult::Forwarded(_) => panic!("first process must own runtime"),
    };
    let control_port = Arc::new(RecordingControlPort::default());
    lease
        .install_control_port(control_port.clone())
        .expect("control port");
    let connection = lease.mark_ready("http://127.0.0.1:61235").expect("ready");
    let discovery = std::fs::read_to_string(workspace.path().join("runtime.json"))
        .expect("published discovery");
    assert!(!discovery.contains("appBridgeCredential"));
    assert!(workspace.path().join("owner.credential").exists());
    let activation_arguments = vec!["--file".into(), "/tmp/note.md".into()];
    let forwarded = coordinator
        .acquire_or_forward(ActivationRequest {
            arguments: activation_arguments.clone(),
            ..Default::default()
        })
        .expect("second activation");
    assert!(matches!(forwarded, AcquireResult::Forwarded(_)));
    assert_eq!(*control_port.activations.lock(), vec![activation_arguments]);
    assert_eq!(connection.generation_nonce, lease.generation_nonce());
    lease
        .begin_quit(QuitRequest::default())
        .expect("ordered quit");
    assert!(!workspace.path().join("runtime.json").exists());
    let replacement = coordinator
        .acquire_or_forward(ActivationRequest::default())
        .expect("replacement owner after clean shutdown");
    assert!(matches!(replacement, AcquireResult::Owner(_)));
}

#[test]
fn background_start_joins_starting_generation_without_foreground_activation() {
    let workspace = tempfile::tempdir().expect("runtime workspace");
    let coordinator = AppRuntimeCoordinator::new(workspace.path(), |_| Ok(()));
    let mut lease = match coordinator
        .acquire_or_forward(ActivationRequest::default())
        .expect("first owner")
    {
        AcquireResult::Owner(lease) => lease,
        AcquireResult::Forwarded(_) => panic!("first process must own runtime"),
    };
    let broker = coordinator.clone();
    let background = std::thread::spawn(move || {
        broker.acquire_or_forward(ActivationRequest {
            background: true,
            ..Default::default()
        })
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    let control_port = Arc::new(RecordingControlPort::default());
    lease
        .install_control_port(control_port.clone())
        .expect("control port replays startup-time events");
    lease
        .mark_ready("http://127.0.0.1:61238")
        .expect("ready generation");
    let joined = background
        .join()
        .expect("background start thread")
        .expect("background start joins starting generation");
    assert!(matches!(joined, AcquireResult::Forwarded(_)));
    assert!(control_port.activations.lock().is_empty());
}

#[test]
fn ordered_quit_is_terminal_for_observed_generation_but_not_fresh_coordinator() {
    let workspace = tempfile::tempdir().expect("runtime workspace");
    let launcher_calls = Arc::new(AtomicUsize::new(0));
    let launcher_calls_for_coordinator = Arc::clone(&launcher_calls);
    let coordinator = AppRuntimeCoordinator::new(workspace.path(), move |_| {
        launcher_calls_for_coordinator.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    let mut lease = match coordinator
        .acquire_or_forward(ActivationRequest::default())
        .expect("first owner")
    {
        AcquireResult::Owner(lease) => lease,
        AcquireResult::Forwarded(_) => panic!("first process must own runtime"),
    };
    let generation = lease
        .mark_ready("http://127.0.0.1:61236")
        .expect("ready generation");
    let cancelled = AtomicBool::new(false);
    let observed = coordinator
        .ensure_ready(
            ActivationRequest::default(),
            std::time::Instant::now() + std::time::Duration::from_secs(1),
            &cancelled,
        )
        .expect("observe ready generation");
    assert_eq!(observed.generation_nonce, generation.generation_nonce);
    assert_eq!(launcher_calls.load(Ordering::SeqCst), 0);

    lease
        .begin_quit(QuitRequest::default())
        .expect("ordered quit");

    let fresh_coordinator = AppRuntimeCoordinator::new(workspace.path(), |_| Ok(()));
    let mut fresh_lease = match fresh_coordinator
        .acquire_or_forward(ActivationRequest::default())
        .expect("fresh coordinator may own later generation")
    {
        AcquireResult::Owner(lease) => lease,
        AcquireResult::Forwarded(_) => panic!("fresh coordinator must own later generation"),
    };
    let fresh_generation = fresh_lease
        .mark_ready("http://127.0.0.1:61237")
        .expect("fresh ready generation");

    let terminal_while_new_generation_is_ready = coordinator.ensure_ready(
        ActivationRequest::default(),
        std::time::Instant::now() + std::time::Duration::from_secs(1),
        &cancelled,
    );
    assert!(matches!(
        terminal_while_new_generation_is_ready,
        Err(RuntimeCoordinatorError::Quitting)
    ));
    assert_eq!(launcher_calls.load(Ordering::SeqCst), 0);

    let fresh_observed = fresh_coordinator
        .ensure_ready(
            ActivationRequest::default(),
            std::time::Instant::now() + std::time::Duration::from_secs(1),
            &cancelled,
        )
        .expect("fresh coordinator observes generation two");
    assert_eq!(
        fresh_observed.generation_nonce,
        fresh_generation.generation_nonce
    );
    fresh_lease
        .begin_quit(QuitRequest::default())
        .expect("fresh generation ordered quit");

    let terminal_after_new_generation_quit = coordinator.ensure_ready(
        ActivationRequest::default(),
        std::time::Instant::now() + std::time::Duration::from_secs(1),
        &cancelled,
    );
    assert!(matches!(
        terminal_after_new_generation_quit,
        Err(RuntimeCoordinatorError::Quitting)
    ));
    assert_eq!(launcher_calls.load(Ordering::SeqCst), 0);
}
