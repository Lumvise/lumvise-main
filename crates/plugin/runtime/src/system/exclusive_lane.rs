//! Signed, generic exclusive-invocation admission before process locking.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lumvise_plugin_package::{
    ExclusiveLaneOperation, ExclusiveLanePolicy, ExportDescriptor, InvocationAdmissionPolicy,
};
use lumvise_plugin_protocol::WireOutcome;
use serde_json::Value;
use tokio::sync::Notify;

use crate::{PluginInvocationContext, PluginRuntimeError};

/// Plugin-scoped host registry for signed exclusive invocation lanes.
pub struct ExclusiveInvocationLanes {
    lanes: Mutex<HashMap<String, LaneState>>,
    async_changed: Arc<Notify>,
}

#[derive(Default)]
struct LaneState {
    active: Option<ActiveLease>,
    queue: VecDeque<QueuedLease>,
    next_ticket: u64,
}

#[derive(Clone)]
struct ActiveLease {
    plugin_id: String,
    owner_id: String,
    session_id: String,
    started_at: Instant,
}

#[derive(Clone)]
struct QueuedLease {
    ticket: u64,
    plugin_id: String,
    owner_id: String,
    session_id: String,
    enqueued_at: Instant,
}

pub(crate) struct LaneInvocation {
    policy: Option<ExclusiveLanePolicy>,
    acquired_session_id: Option<String>,
}

/// Caller-visible generic lane state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExclusiveLaneSnapshot {
    /// Signed lane identity.
    pub lane_id: String,
    /// Active session, when the lane is occupied.
    pub active_session_id: Option<String>,
    /// Elapsed time for the active lease.
    pub active_elapsed_ms: Option<u128>,
    /// Number of FIFO waiters.
    pub queued: usize,
    /// Caller-relative state: `active` or `queued`.
    pub caller_status: Option<String>,
    /// One-based FIFO position for a queued caller.
    pub caller_position: Option<usize>,
}

impl ExclusiveInvocationLanes {
    /// Creates an empty plugin-scoped lane registry.
    pub fn new() -> Self {
        Self {
            lanes: Mutex::new(HashMap::new()),
            async_changed: Arc::new(Notify::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn admit(
        &self,
        plugin_id: &str,
        export: &ExportDescriptor,
        input: &Value,
    ) -> Result<LaneInvocation, PluginRuntimeError> {
        let Some(policy) = exclusive_lane_policy(plugin_id, export)? else {
            return Ok(LaneInvocation::none());
        };
        if policy.operation == ExclusiveLaneOperation::ReleaseOnOutput {
            return Ok(LaneInvocation::release_only(policy));
        }
        let request = LaneRequest::from_input(plugin_id, &policy, input)?;
        let mut lanes = self.lock_lanes()?;
        let state = lanes
            .entry(lane_key(plugin_id, &policy.lane_id))
            .or_default();
        if request.replace && replace_active(state, &request, &policy.lane_id)? {
            return Ok(LaneInvocation::acquired(policy, request.session_id));
        }
        if state.active.is_none() && state.queue.is_empty() {
            state.activate(&request);
            return Ok(LaneInvocation::acquired(policy, request.session_id));
        }
        Err(lane_error(
            &policy.lane_id,
            &request.session_id,
            "free lane",
        ))
    }

    pub(crate) async fn admit_async(
        &self,
        plugin_id: &str,
        export: &ExportDescriptor,
        input: &Value,
        context: &PluginInvocationContext,
        maximum_duration: Duration,
    ) -> Result<LaneInvocation, PluginRuntimeError> {
        let Some(policy) = exclusive_lane_policy(plugin_id, export)? else {
            return Ok(LaneInvocation::none());
        };
        if policy.operation == ExclusiveLaneOperation::ReleaseOnOutput {
            return Ok(LaneInvocation::release_only(policy));
        }
        let request = LaneRequest::from_input(plugin_id, &policy, input)?;
        self.acquire_async(
            &lane_key(plugin_id, &policy.lane_id),
            &policy.lane_id,
            &request,
            policy.max_queue_depth,
            context,
            maximum_duration,
        )
        .await?;
        Ok(LaneInvocation::acquired(policy, request.session_id))
    }

    pub(crate) fn finish(
        &self,
        plugin_id: &str,
        invocation: LaneInvocation,
        result: &Result<WireOutcome, PluginRuntimeError>,
    ) {
        let Some(policy) = invocation.policy else {
            return;
        };
        let Ok(outcome) = result else {
            if let Some(session_id) = invocation.acquired_session_id {
                self.release_session(plugin_id, &policy.lane_id, &session_id);
            }
            return;
        };
        let WireOutcome::Succeeded { value } = outcome else {
            if let Some(session_id) = invocation.acquired_session_id {
                self.release_session(plugin_id, &policy.lane_id, &session_id);
            }
            return;
        };
        let response_session = value
            .pointer(&policy.response_session_pointer)
            .and_then(Value::as_str);
        if let (Some(provisional), Some(actual)) =
            (invocation.acquired_session_id.as_deref(), response_session)
        {
            self.rename_session(plugin_id, &policy.lane_id, provisional, actual);
        }
        if exclusive_lane_output_is_terminal(&policy, value)
            && let Some(session_id) = response_session.or(invocation.acquired_session_id.as_deref())
        {
            self.release_session(plugin_id, &policy.lane_id, session_id);
        }
    }

    pub(crate) fn release_plugin(&self, plugin_id: &str) {
        let Ok(mut lanes) = self.lanes.lock() else {
            return;
        };
        let prefix = format!("{plugin_id}\0");
        for (key, state) in lanes.iter_mut().filter(|(key, _)| key.starts_with(&prefix)) {
            if state
                .active
                .as_ref()
                .is_some_and(|lease| lease.plugin_id == plugin_id)
            {
                state.active = None;
            }
            state.queue.retain(|lease| lease.plugin_id != plugin_id);
            let _ = key;
        }
        self.async_changed.notify_waiters();
    }

    /// Cancels the active lease in one plugin-scoped lane.
    pub fn cancel(&self, plugin_id: &str, lane_id: &str) -> Result<(), PluginRuntimeError> {
        let mut lanes = self.lock_lanes()?;
        let state = lanes
            .get_mut(&lane_key(plugin_id, lane_id))
            .ok_or_else(|| lane_error(lane_id, "cancel", "active signed exclusive lane"))?;
        if state.active.take().is_none() {
            return Err(lane_error(
                lane_id,
                "cancel",
                "active signed exclusive lane",
            ));
        }
        self.async_changed.notify_waiters();
        Ok(())
    }

    /// Returns caller-relative state for one plugin-scoped lane.
    pub fn snapshot(
        &self,
        plugin_id: &str,
        lane_id: &str,
        owner_id: &str,
        session_id: Option<&str>,
    ) -> Result<ExclusiveLaneSnapshot, PluginRuntimeError> {
        let lanes = self.lock_lanes()?;
        let state = lanes.get(&lane_key(plugin_id, lane_id));
        Ok(snapshot(lane_id, state, owner_id, session_id))
    }

    async fn acquire_async(
        &self,
        key: &str,
        lane_id: &str,
        request: &LaneRequest,
        max_queue_depth: u32,
        context: &PluginInvocationContext,
        maximum_duration: Duration,
    ) -> Result<(), PluginRuntimeError> {
        let queued = {
            let mut lanes = self.lock_lanes()?;
            let state = lanes.entry(key.to_owned()).or_default();
            if request.replace && replace_active(state, request, lane_id)? {
                return Ok(());
            }
            if state.active.is_none() && state.queue.is_empty() {
                state.activate(request);
                return Ok(());
            }
            if !request.queue {
                return Err(lane_error(
                    lane_id,
                    &request.session_id,
                    "free lane or queue=true",
                ));
            }
            if state.queue.len() >= max_queue_depth as usize {
                return Err(lane_error(
                    lane_id,
                    &state.queue.len().to_string(),
                    "queue below signed maximum",
                ));
            }
            state.enqueue(request)
        };
        let timeout_deadline = request.timeout.map(|timeout| queued.enqueued_at + timeout);
        loop {
            let remaining = match context.ensure_active(&request.plugin_id, maximum_duration) {
                Ok(remaining) => remaining,
                Err(error) => {
                    self.remove_queued(key, queued.ticket);
                    return Err(error);
                }
            };
            let notified = self.async_changed.notified();
            {
                let mut lanes = self.lock_lanes()?;
                let Some(state) = lanes.get_mut(key) else {
                    return Err(cancelled(&request.plugin_id, lane_id));
                };
                if !state.has_ticket(queued.ticket) {
                    return Err(cancelled(&request.plugin_id, lane_id));
                }
                if state.can_activate(queued.ticket) {
                    state.pop_ticket(queued.ticket);
                    state.activate_queued(&queued);
                    return Ok(());
                }
            }
            let wait_for = timeout_deadline
                .map(|deadline| {
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(remaining)
                })
                .unwrap_or(remaining);
            if wait_for.is_zero() {
                self.remove_queued(key, queued.ticket);
                return Err(PluginRuntimeError::ExclusiveLaneTimeout {
                    lane_id: lane_id.into(),
                });
            }
            let cancellation = context.cancellation();
            tokio::select! {
                _ = cancellation.cancelled() => {}
                _ = tokio::time::sleep(wait_for) => {}
                _ = notified => {}
            }
        }
    }

    fn lock_lanes(&self) -> Result<MutexGuard<'_, HashMap<String, LaneState>>, PluginRuntimeError> {
        self.lanes.lock().map_err(|_| poisoned_lane())
    }

    fn remove_queued(&self, key: &str, ticket: u64) {
        if let Ok(mut lanes) = self.lanes.lock() {
            if let Some(state) = lanes.get_mut(key) {
                state.remove_ticket(ticket);
            }
            self.async_changed.notify_waiters();
        }
    }

    fn rename_session(&self, plugin_id: &str, lane_id: &str, previous: &str, actual: &str) {
        let Ok(mut lanes) = self.lanes.lock() else {
            return;
        };
        if let Some(active) = lanes
            .get_mut(&lane_key(plugin_id, lane_id))
            .and_then(|state| state.active.as_mut())
            && active.session_id == previous
        {
            active.session_id = actual.into();
        }
    }

    fn release_session(&self, plugin_id: &str, lane_id: &str, session_id: &str) {
        let Ok(mut lanes) = self.lanes.lock() else {
            return;
        };
        if let Some(state) = lanes.get_mut(&lane_key(plugin_id, lane_id))
            && state
                .active
                .as_ref()
                .is_some_and(|lease| lease.session_id == session_id)
        {
            state.active = None;
            self.async_changed.notify_waiters();
        }
    }
}

impl Default for ExclusiveInvocationLanes {
    fn default() -> Self {
        Self::new()
    }
}

struct LaneRequest {
    plugin_id: String,
    owner_id: String,
    session_id: String,
    queue: bool,
    replace: bool,
    timeout: Option<Duration>,
}

impl LaneRequest {
    fn from_input(
        plugin_id: &str,
        policy: &ExclusiveLanePolicy,
        input: &Value,
    ) -> Result<Self, PluginRuntimeError> {
        let owner_id =
            string_field(input, &policy.owner_argument).unwrap_or_else(|| "default".into());
        let session_id = string_field(input, &policy.session_argument)
            .unwrap_or_else(|| format!("pending-{plugin_id}-{owner_id}"));
        let timeout = duration_field(input, policy)?;
        Ok(Self {
            plugin_id: plugin_id.into(),
            owner_id,
            session_id,
            queue: bool_field(input, &policy.queue_argument).unwrap_or(true),
            replace: bool_field(input, &policy.replace_argument).unwrap_or(false),
            timeout,
        })
    }
}

fn exclusive_lane_policy(
    plugin_id: &str,
    export: &ExportDescriptor,
) -> Result<Option<ExclusiveLanePolicy>, PluginRuntimeError> {
    let _ = plugin_id;
    Ok(export.admission.as_ref().map(|admission| match admission {
        InvocationAdmissionPolicy::ExclusiveLane(policy) => policy.clone(),
    }))
}

fn exclusive_lane_output_is_terminal(policy: &ExclusiveLanePolicy, output: &Value) -> bool {
    output
        .pointer(&policy.terminal_pointer)
        .and_then(Value::as_str)
        .is_some_and(|value| {
            policy
                .terminal_values
                .iter()
                .any(|terminal| terminal == value)
        })
}

impl LaneInvocation {
    fn none() -> Self {
        Self {
            policy: None,
            acquired_session_id: None,
        }
    }
    fn release_only(policy: ExclusiveLanePolicy) -> Self {
        Self {
            policy: Some(policy),
            acquired_session_id: None,
        }
    }
    fn acquired(policy: ExclusiveLanePolicy, session_id: String) -> Self {
        Self {
            policy: Some(policy),
            acquired_session_id: Some(session_id),
        }
    }
}

impl LaneState {
    fn activate(&mut self, request: &LaneRequest) {
        self.active = Some(ActiveLease {
            plugin_id: request.plugin_id.clone(),
            owner_id: request.owner_id.clone(),
            session_id: request.session_id.clone(),
            started_at: Instant::now(),
        });
    }
    fn enqueue(&mut self, request: &LaneRequest) -> QueuedLease {
        let lease = QueuedLease {
            ticket: self.next_ticket,
            plugin_id: request.plugin_id.clone(),
            owner_id: request.owner_id.clone(),
            session_id: request.session_id.clone(),
            enqueued_at: Instant::now(),
        };
        self.next_ticket += 1;
        self.queue.push_back(lease.clone());
        lease
    }
    fn can_activate(&self, ticket: u64) -> bool {
        self.active.is_none()
            && self
                .queue
                .front()
                .is_some_and(|lease| lease.ticket == ticket)
    }
    fn has_ticket(&self, ticket: u64) -> bool {
        self.queue.iter().any(|lease| lease.ticket == ticket)
    }
    fn pop_ticket(&mut self, ticket: u64) {
        if self
            .queue
            .front()
            .is_some_and(|lease| lease.ticket == ticket)
        {
            self.queue.pop_front();
        }
    }
    fn activate_queued(&mut self, queued: &QueuedLease) {
        self.active = Some(ActiveLease {
            plugin_id: queued.plugin_id.clone(),
            owner_id: queued.owner_id.clone(),
            session_id: queued.session_id.clone(),
            started_at: Instant::now(),
        });
    }
    fn remove_ticket(&mut self, ticket: u64) {
        self.queue.retain(|lease| lease.ticket != ticket);
    }
}

fn replace_active(
    state: &mut LaneState,
    request: &LaneRequest,
    lane_id: &str,
) -> Result<bool, PluginRuntimeError> {
    let Some(active) = &state.active else {
        return Ok(false);
    };
    if active.owner_id != request.owner_id {
        return Err(lane_error(
            lane_id,
            &request.owner_id,
            "same owner for replacement",
        ));
    }
    state.active = None;
    state.activate(request);
    Ok(true)
}

fn snapshot(
    lane_id: &str,
    state: Option<&LaneState>,
    owner_id: &str,
    session_id: Option<&str>,
) -> ExclusiveLaneSnapshot {
    let active = state.and_then(|state| state.active.as_ref());
    let caller = state.and_then(|state| {
        state.queue.iter().position(|lease| {
            lease.owner_id == owner_id && session_id.is_none_or(|id| lease.session_id == id)
        })
    });
    ExclusiveLaneSnapshot {
        lane_id: lane_id.into(),
        active_session_id: active.map(|lease| lease.session_id.clone()),
        active_elapsed_ms: active.map(|lease| lease.started_at.elapsed().as_millis()),
        queued: state.map_or(0, |state| state.queue.len()),
        caller_status: caller.map(|_| "queued".into()).or_else(|| {
            active
                .filter(|lease| {
                    lease.owner_id == owner_id && session_id.is_none_or(|id| lease.session_id == id)
                })
                .map(|_| "active".into())
        }),
        caller_position: caller.map(|position| position + 1),
    }
}

fn string_field(input: &Value, field: &str) -> Option<String> {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
fn bool_field(input: &Value, field: &str) -> Option<bool> {
    input.get(field).and_then(Value::as_bool)
}
fn duration_field(
    input: &Value,
    policy: &ExclusiveLanePolicy,
) -> Result<Option<Duration>, PluginRuntimeError> {
    let selected = input
        .get(&policy.timeout_ms_argument)
        .filter(|value| !value.is_null())
        .map(|value| {
            value.as_u64().ok_or_else(|| {
                lane_error(
                    &policy.lane_id,
                    &value.to_string(),
                    "unsigned timeout milliseconds",
                )
            })
        })
        .transpose()?
        .unwrap_or(policy.default_timeout_ms);
    if selected > policy.max_timeout_ms {
        return Err(lane_error(
            &policy.lane_id,
            &selected.to_string(),
            &format!("timeout at most {} milliseconds", policy.max_timeout_ms),
        ));
    }
    Ok(Some(Duration::from_millis(selected)))
}
fn lane_error(lane_id: &str, value: &str, expected: &str) -> PluginRuntimeError {
    PluginRuntimeError::ExclusiveLaneDenied {
        lane_id: lane_id.into(),
        value: value.into(),
        expected: expected.into(),
    }
}
fn poisoned_lane() -> PluginRuntimeError {
    PluginRuntimeError::ExclusiveLanePoisoned
}

fn lane_key(plugin_id: &str, lane_id: &str) -> String {
    format!("{plugin_id}\0{lane_id}")
}

fn cancelled(plugin_id: &str, lane_id: &str) -> PluginRuntimeError {
    PluginRuntimeError::ExclusiveLaneCancelled {
        plugin_id: plugin_id.into(),
        lane_id: lane_id.into(),
    }
}

#[cfg(test)]
mod tests {
    use lumvise_plugin_package::{ExecutionMode, ExportSurface};
    use serde_json::json;

    use super::*;

    #[test]
    fn signed_lane_denies_overlap_and_releases_on_terminal_output() {
        let lanes = ExclusiveInvocationLanes::new();
        let export = acquire_export();
        let first = lanes
            .admit("plugin-a", &export, &request("owner-a", "session-a"))
            .expect("first admission");

        let denied = match lanes.admit("plugin-a", &export, &request("owner-b", "session-b")) {
            Ok(_) => panic!("overlapping invocation admitted"),
            Err(error) => error,
        };
        assert!(matches!(
            denied,
            PluginRuntimeError::ExclusiveLaneDenied { .. }
        ));

        lanes.finish(
            "plugin-a",
            first,
            &Ok(WireOutcome::Succeeded {
                value: json!({"session_id": "session-a", "phase": "completed"}),
            }),
        );
        lanes
            .admit("plugin-a", &export, &request("owner-b", "session-b"))
            .expect("terminal output releases lane");
    }

    #[test]
    fn identical_lane_ids_are_isolated_by_plugin_identity() {
        let lanes = ExclusiveInvocationLanes::new();
        let export = acquire_export();
        lanes
            .admit("plugin-a", &export, &request("owner", "session-a"))
            .expect("plugin A admission");
        lanes
            .admit("plugin-b", &export, &request("owner", "session-b"))
            .expect("plugin B isolated admission");
    }

    #[test]
    fn replacement_does_not_leapfrog_an_existing_fifo_waiter() {
        let mut state = LaneState::default();
        state.queue.push_back(QueuedLease {
            ticket: 1,
            plugin_id: "plugin-a".into(),
            owner_id: "owner-a".into(),
            session_id: "queued".into(),
            enqueued_at: Instant::now(),
        });
        let replacement = LaneRequest {
            plugin_id: "plugin-a".into(),
            owner_id: "owner-a".into(),
            session_id: "replacement".into(),
            queue: true,
            replace: true,
            timeout: Some(Duration::from_millis(1)),
        };

        assert!(!replace_active(&mut state, &replacement, "assistant").unwrap());
        assert!(state.active.is_none());
        assert_eq!(state.queue.front().unwrap().session_id, "queued");
    }

    fn acquire_export() -> ExportDescriptor {
        ExportDescriptor {
            description: String::new(),
            id: "start".into(),
            name: "Start".into(),
            surface: ExportSurface::McpTool,
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            admission: Some(InvocationAdmissionPolicy::ExclusiveLane(
                ExclusiveLanePolicy {
                    lane_id: "assistant".into(),
                    operation: ExclusiveLaneOperation::Acquire,
                    owner_argument: "owner_id".into(),
                    session_argument: "session_id".into(),
                    queue_argument: "queue".into(),
                    replace_argument: "replace".into(),
                    timeout_ms_argument: "timeout_ms".into(),
                    default_timeout_ms: 1_000,
                    max_timeout_ms: 5_000,
                    max_queue_depth: 8,
                    response_session_pointer: "/session_id".into(),
                    terminal_pointer: "/phase".into(),
                    terminal_values: vec!["completed".into()],
                },
            )),
            execution: ExecutionMode::Foreground,
        }
    }

    fn request(owner_id: &str, session_id: &str) -> Value {
        json!({
            "owner_id": owner_id,
            "session_id": session_id,
            "queue": false,
            "replace": false,
        })
    }
}
