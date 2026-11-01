use super::*;
use lumvise_app_core::{OwnerLease, RuntimeConnection};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

struct PublishingOwner {
    directory: tempfile::TempDir,
    coordinator: AppRuntimeCoordinator,
    lease: OwnerLease,
    port: Arc<RecordingControlPort>,
}

impl PublishingOwner {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let coordinator = AppRuntimeCoordinator::new(directory.path(), |_| Ok(()));
        let lease = acquire_initial_lease(&coordinator);
        let port = Arc::new(RecordingControlPort::default());
        lease.install_control_port(port.clone()).unwrap();
        Self {
            directory,
            coordinator,
            lease,
            port,
        }
    }

    fn launch_again(&self) -> Receiver<Result<AcquireResult, RuntimeCoordinatorError>> {
        let coordinator = self.coordinator.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = coordinator.acquire_or_forward(ActivationRequest {
                arguments: vec!["--file".into(), "/tmp/startup-note.md".into()],
                background: false,
            });
            sender.send(result).unwrap();
        });
        receiver
    }

    fn publish_after_launch(&mut self, filename: &str) -> RuntimeConnection {
        let path = self.directory.path().join(filename);
        let original = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let receiver = self.launch_again();
        let early = receiver.recv_timeout(Duration::from_millis(150));
        std::fs::write(path, original).unwrap();
        self.lease.mark_ready("http://127.0.0.1:61239").unwrap();
        let error = early.as_ref().ok().and_then(|result| result.as_ref().err());
        assert!(
            matches!(early, Err(RecvTimeoutError::Timeout)),
            "early result: {error:?}"
        );
        receive_forwarded_activation(receiver)
    }
}

fn acquire_initial_lease(coordinator: &AppRuntimeCoordinator) -> OwnerLease {
    let AcquireResult::Owner(lease) = coordinator
        .acquire_or_forward(ActivationRequest::default())
        .unwrap()
    else {
        panic!("expected the initial runtime owner")
    };
    lease
}

fn receive_forwarded_activation(
    receiver: Receiver<Result<AcquireResult, RuntimeCoordinatorError>>,
) -> RuntimeConnection {
    let AcquireResult::Forwarded(connection) = receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("launch after publication")
        .expect("forwarded activation")
    else {
        panic!("expected forwarding to the existing owner")
    };
    connection
}

#[test]
fn launch_waits_for_owner_discovery_and_credential_publication() {
    for filename in ["runtime.json", "owner.credential"] {
        let mut owner = PublishingOwner::new();
        let connection = owner.publish_after_launch(filename);
        assert_eq!(connection.generation_nonce, owner.lease.generation_nonce());
        assert_eq!(
            *owner.port.activations.lock(),
            vec![vec!["--file".to_owned(), "/tmp/startup-note.md".to_owned(),]]
        );
    }
}
