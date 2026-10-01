//! Owns the app's transient LLM activity history and change notification.
//! Producers publish lifecycle transitions; the workspace reads `wait_for_changes`.
//! Prompts, responses, and scheduling decisions stay with their existing owners.

use lumvise_neural_core::llm_providers::{LlmExecutionControl, LlmFailure, LlmFailureCode};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const FINISHED_HISTORY: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivityStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl ActivityStatus {
    fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivityKind {
    Knowledge,
    Assistant,
    Llm,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ActivityEntry {
    pub id: String,
    pub project_root: Option<String>,
    pub title: String,
    pub kind: ActivityKind,
    pub status: ActivityStatus,
    pub semantic_element_id: Option<String>,
    pub artifact_id: Option<String>,
    pub detail: Option<String>,
}

impl ActivityEntry {
    fn llm(source: &str, provider: &str) -> Self {
        let (kind, title) = if source == "builtin.assistant" {
            (ActivityKind::Assistant, "Assistant")
        } else {
            (ActivityKind::Llm, "LLM request")
        };
        Self {
            id: format!("llm-{}", uuid::Uuid::new_v4()),
            project_root: None,
            title: format!("{title} · {provider}"),
            kind,
            status: ActivityStatus::Queued,
            semantic_element_id: None,
            artifact_id: None,
            detail: None,
        }
    }
}

#[derive(Default)]
pub(crate) struct WorkspaceActivity {
    state: Mutex<ActivityState>,
    changed: Condvar,
}

#[derive(Default)]
struct ActivityState {
    revision: u64,
    entries: VecDeque<ActivityEntry>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ActivitySnapshot {
    pub revision: u64,
    // A timeout carries no repeated history. A missing/ahead cursor gets a full snapshot.
    pub entries: Option<Vec<ActivityEntry>>,
}

impl WorkspaceActivity {
    /// Starts an app-owned provider call, e.g. `activity.track_llm("builtin.assistant", "codex")`.
    pub(crate) fn track_llm(self: &Arc<Self>, source: &str, provider: &str) -> ActivityTicket {
        let entry = ActivityEntry::llm(source, provider);
        let id = entry.id.clone();
        self.record(entry);
        ActivityTicket {
            activity: Arc::clone(self),
            id,
        }
    }

    /// Records a newly admitted project job using its existing identity and scope.
    /// Example: `activity.record(entry)` after the execution broker accepts work.
    pub(crate) fn record(&self, entry: ActivityEntry) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.entries.iter().any(|existing| existing.id == entry.id) {
            return;
        }
        state.entries.push_back(entry);
        self.publish(&mut state);
    }

    /// Mirrors a scheduler transition; terminal entries reject late worker updates.
    /// Example: `activity.update(job_id, ActivityStatus::Running, None)`.
    pub(crate) fn update(&self, id: &str, status: ActivityStatus, detail: Option<String>) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let Some(index) = state.entries.iter().position(|entry| entry.id == id) else {
            return;
        };
        let entry = &state.entries[index];
        if entry.status.is_terminal() || (entry.status == status && entry.detail == detail) {
            return;
        }
        let mut entry = state.entries.remove(index).expect("located activity entry");
        entry.status = status;
        entry.detail = detail;
        state.entries.push_back(entry);
        self.publish(&mut state);
    }

    /// Waits for a changed cursor and returns the current project plus app-wide calls.
    /// Example: `activity.wait_for_changes(root, Some(revision), Duration::from_secs(15))`.
    pub(crate) fn wait_for_changes(
        &self,
        project_root: &str,
        after_revision: Option<u64>,
        timeout: Duration,
    ) -> ActivitySnapshot {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let (state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| {
                after_revision == Some(state.revision)
            })
            .unwrap_or_else(|error| error.into_inner());
        let entries = (after_revision != Some(state.revision)).then(|| {
            state
                .entries
                .iter()
                .rev()
                .filter(|entry| {
                    entry
                        .project_root
                        .as_deref()
                        .is_none_or(|root| root == project_root)
                })
                .cloned()
                .collect()
        });
        ActivitySnapshot {
            revision: state.revision,
            entries,
        }
    }

    fn publish(&self, state: &mut ActivityState) {
        let finished = state
            .entries
            .iter()
            .filter(|entry| entry.status.is_terminal())
            .count();
        let mut discard = finished.saturating_sub(FINISHED_HISTORY);
        state.entries.retain(|entry| {
            if discard == 0 || !entry.status.is_terminal() {
                return true;
            }
            discard -= 1;
            false
        });
        state.revision += 1;
        self.changed.notify_all();
    }
}

pub(crate) struct ActivityTicket {
    activity: Arc<WorkspaceActivity>,
    id: String,
}

impl ActivityTicket {
    /// Reports running only when the provider worker starts this call.
    /// Example: `executor.complete(provider, request, ticket.control(control))`.
    pub(crate) fn control(&self, control: LlmExecutionControl) -> LlmExecutionControl {
        let activity = Arc::clone(&self.activity);
        let id = self.id.clone();
        control.on_start(Arc::new(move || {
            activity.update(&id, ActivityStatus::Running, None)
        }))
    }

    /// Marks a stream as started when its provider is invoked, e.g. `ticket.started()`.
    pub(crate) fn started(&self) {
        self.activity
            .update(&self.id, ActivityStatus::Running, None);
    }

    /// Finishes a provider call without retaining its response, e.g. `ticket.finish(&result)`.
    pub(crate) fn finish<T>(&self, result: &Result<T, LlmFailure>) {
        let (status, detail) = match result {
            Ok(_) => (ActivityStatus::Succeeded, None),
            Err(failure) if failure.code == LlmFailureCode::Cancelled => {
                (ActivityStatus::Cancelled, Some(failure.message.clone()))
            }
            Err(failure) => (ActivityStatus::Failed, Some(failure.message.clone())),
        };
        self.activity.update(&self.id, status, detail);
    }

    /// Records streaming outcomes while preserving the endpoint's original error type.
    /// Example: `ticket.finish_stream(&provider_result)`.
    pub(crate) fn finish_stream<T>(&self, result: &lumvise_neural_core::Result<T>) {
        let observed = result.as_ref().map_err(|error| {
            let code = if matches!(
                error,
                lumvise_neural_core::NeuralError::ProcessCancelled { .. }
            ) {
                LlmFailureCode::Cancelled
            } else {
                LlmFailureCode::ProviderRejected
            };
            LlmFailure::new(code, error.to_string())
        });
        self.finish(&observed);
    }
}

impl Drop for ActivityTicket {
    fn drop(&mut self) {
        self.activity.update(
            &self.id,
            ActivityStatus::Cancelled,
            Some("abandoned before completion".into()),
        );
    }
}

#[cfg(test)]
mod tests;
