//! Deterministic relational commit pause for two real cooperating plugin hosts.
use super::*;
use std::sync::mpsc::{self, Receiver, Sender};

pub(super) struct SourceDescriptorCommitGate {
    entered: Sender<()>,
    resume: Receiver<()>,
}

pub struct PausedSourceDescriptorCommit {
    entered: Receiver<()>,
    resume: Sender<()>,
}

impl MemoryCapabilityBroker {
    pub fn cooperating_instance(&self) -> Self {
        Self::sharing(self.persistence.clone())
    }

    pub fn pause_next_source_commit(&self) -> PausedSourceDescriptorCommit {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        *self.source_commit_gate.lock().unwrap() = Some(SourceDescriptorCommitGate {
            entered: entered_tx,
            resume: resume_rx,
        });
        PausedSourceDescriptorCommit {
            entered: entered_rx,
            resume: resume_tx,
        }
    }
}

impl SourceDescriptorCommitGate {
    pub(super) fn pause(self) -> Result<(), HostCapabilityError> {
        self.entered
            .send(())
            .map_err(|_| host_error("storage.plugin", "source_commit_observer_gone"))?;
        self.resume
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| host_error("storage.plugin", "source_commit_gate_timeout"))
    }
}

impl PausedSourceDescriptorCommit {
    pub fn wait(&self) {
        self.entered
            .recv_timeout(Duration::from_secs(3))
            .expect("first source projection must reach its relational commit");
    }

    pub fn release(self) {
        self.resume
            .send(())
            .expect("first source projection must still be paused");
    }
}
