//! Plugin-local FIFO admission, isolated from process lifecycle locks.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::Notify;

use super::export_concurrency_policy::ExportConcurrencyRegistry;
use crate::{PluginInvocationClass, PluginInvocationContext, PluginRuntimeError};

const CANCELLATION_POLL: Duration = Duration::from_millis(10);

#[cfg(test)]
mod cancellation_tests;

pub(crate) struct PluginInvocationAdmission {
    mailboxes: Mutex<HashMap<String, Arc<PluginMailbox>>>,
    maximum_queued: usize,
    pipeline_limit: u32,
}

#[derive(Default)]
struct PluginMailbox {
    state: Mutex<MailboxState>,
    changed: Condvar,
    async_changed: Arc<Notify>,
    plugin_id: String,
}

#[derive(Default)]
struct MailboxState {
    /// Tracks concurrent invocations per export ID.
    executing_by_export: HashMap<String, u32>,
    foreground_queue: VecDeque<u64>,
    background_queue: VecDeque<u64>,
    next_ticket: u64,
    cancelled: u64,
    expired: u64,
    overloaded: u64,
}

pub(crate) struct PluginInvocationPermit {
    mailbox: Arc<PluginMailbox>,
    export_id: String,
}

struct QueuedMailboxTicket<'mailbox> {
    mailbox: &'mailbox PluginMailbox,
    ticket: Option<u64>,
    class: PluginInvocationClass,
    request_id: &'mailbox str,
}

impl Drop for QueuedMailboxTicket<'_> {
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket {
            self.mailbox.remove_queued(
                ticket,
                self.class,
                PluginRuntimeError::InvocationCancelled {
                    plugin_id: self.mailbox.plugin_id.clone(),
                    request_id: self.request_id.to_owned(),
                },
            );
        }
    }
}

/// Lock-local Plugin Invocation admission state and terminal counters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginAdmissionSnapshot {
    /// Target Plugin identity.
    pub plugin_id: String,
    /// Whether one invocation owns process dispatch.
    pub executing: bool,
    /// Number of FIFO mailbox waiters.
    pub queued: usize,
    /// Number of interactive waiters. Foreground dispatch has priority.
    pub foreground_queued: usize,
    /// Number of background waiters, bounded below total mailbox capacity.
    pub background_queued: usize,
    /// Cumulative queued invocations cancelled before dispatch.
    pub cancelled: u64,
    /// Cumulative queued invocations expired before dispatch.
    pub expired: u64,
    /// Cumulative invocations rejected because the mailbox was full.
    pub overloaded: u64,
}

struct AdmissionContext<'a> {
    plugin_id: &'a str,
    export_id: &'a str,
    invocation_id: &'a str,
    owner: &'a str,
    class: PluginInvocationClass,
    context: &'a PluginInvocationContext,
    export_concurrency: &'a ExportConcurrencyRegistry,
    maximum_duration: Duration,
    maximum_queued: usize,
    pipeline_limit: u32,
}

impl<'a> AdmissionContext<'a> {
    fn new(
        plugin_id: &'a str,
        export_id: &'a str,
        context: &'a PluginInvocationContext,
        export_concurrency: &'a ExportConcurrencyRegistry,
        maximum_duration: Duration,
        maximum_queued: usize,
        pipeline_limit: u32,
    ) -> Self {
        Self {
            plugin_id,
            export_id,
            invocation_id: context.request_id(),
            owner: context.owner_id(),
            class: context.class(),
            context,
            export_concurrency,
            maximum_duration,
            maximum_queued,
            pipeline_limit,
        }
    }

    fn ensure_active(&self) -> Result<Duration, PluginRuntimeError> {
        debug_assert_eq!(self.invocation_id, self.context.request_id());
        debug_assert_eq!(self.owner, self.context.owner_id());
        debug_assert_eq!(self.class, self.context.class());
        self.context
            .ensure_active(self.plugin_id, self.maximum_duration)
    }
}

impl PluginInvocationAdmission {
    pub(crate) fn new(maximum_queued: usize, pipeline_limit: u32) -> Self {
        Self {
            mailboxes: Mutex::new(HashMap::new()),
            maximum_queued,
            pipeline_limit,
        }
    }

    pub(crate) fn admit(
        &self,
        plugin_id: &str,
        export_id: &str,
        context: &PluginInvocationContext,
        export_concurrency: &ExportConcurrencyRegistry,
        maximum_duration: Duration,
    ) -> Result<PluginInvocationPermit, PluginRuntimeError> {
        context.ensure_active(plugin_id, maximum_duration)?;
        let mailbox = self.mailbox(plugin_id)?;
        let request = AdmissionContext::new(
            plugin_id,
            export_id,
            context,
            export_concurrency,
            maximum_duration,
            self.maximum_queued,
            self.pipeline_limit,
        );
        mailbox.admit(request)
    }

    pub(crate) async fn admit_async(
        &self,
        plugin_id: &str,
        export_id: &str,
        context: &PluginInvocationContext,
        export_concurrency: &ExportConcurrencyRegistry,
        maximum_duration: Duration,
    ) -> Result<PluginInvocationPermit, PluginRuntimeError> {
        context.ensure_active(plugin_id, maximum_duration)?;
        let mailbox = self.mailbox(plugin_id)?;
        let request = AdmissionContext::new(
            plugin_id,
            export_id,
            context,
            export_concurrency,
            maximum_duration,
            self.maximum_queued,
            self.pipeline_limit,
        );
        mailbox.admit_async(request).await
    }

    pub(crate) fn snapshot(
        &self,
        plugin_id: &str,
    ) -> Result<PluginAdmissionSnapshot, PluginRuntimeError> {
        let mailbox = self.mailbox(plugin_id)?;
        let state = mailbox.lock_state()?;
        Ok(state.snapshot(plugin_id))
    }

    fn mailbox(&self, plugin_id: &str) -> Result<Arc<PluginMailbox>, PluginRuntimeError> {
        let mut mailboxes = self
            .mailboxes
            .lock()
            .map_err(|_| PluginRuntimeError::InvocationAdmissionPoisoned)?;
        if let Some(existing) = mailboxes.get(plugin_id) {
            return Ok(Arc::clone(existing));
        }
        let mailbox = Arc::new(PluginMailbox {
            state: Mutex::new(MailboxState::default()),
            changed: Condvar::new(),
            async_changed: Arc::new(Notify::new()),
            plugin_id: plugin_id.to_owned(),
        });
        mailboxes.insert(plugin_id.to_owned(), Arc::clone(&mailbox));
        Ok(mailbox)
    }
}

impl PluginMailbox {
    fn admit(
        self: &Arc<Self>,
        request: AdmissionContext<'_>,
    ) -> Result<PluginInvocationPermit, PluginRuntimeError> {
        let mut state = self.lock_state()?;
        if let Some(permit) = self.try_fast_path(&mut state, &request) {
            return Ok(permit);
        }
        let ticket = self.enqueue_or_reject(&mut state, &request)?;
        self.wait_for_turn(state, ticket, request)
    }

    async fn admit_async(
        self: &Arc<Self>,
        request: AdmissionContext<'_>,
    ) -> Result<PluginInvocationPermit, PluginRuntimeError> {
        let ticket = {
            let mut state = self.lock_state()?;
            if let Some(permit) = self.try_fast_path(&mut state, &request) {
                return Ok(permit);
            }
            self.enqueue_or_reject(&mut state, &request)?
        };
        let mut queued = QueuedMailboxTicket {
            mailbox: self,
            ticket: Some(ticket),
            class: request.class,
            request_id: request.invocation_id,
        };
        let result = self.wait_async_for_turn(request, ticket).await;
        queued.ticket = None;
        result
    }

    fn wait_for_turn(
        self: &Arc<Self>,
        mut state: MutexGuard<'_, MailboxState>,
        ticket: u64,
        request: AdmissionContext<'_>,
    ) -> Result<PluginInvocationPermit, PluginRuntimeError> {
        loop {
            let remaining = match request.ensure_active() {
                Ok(remaining) => remaining,
                Err(error) => {
                    return Err(self.remove_terminal(state, ticket, request.class, error));
                }
            };
            let max_concurrent = request
                .export_concurrency
                .get_policy(request.export_id)
                .max_concurrent();
            if let Some(permit) =
                self.dispatch_if_ready(&mut state, ticket, &request, max_concurrent)
            {
                return Ok(permit);
            }
            state = self.wait(state, remaining.min(CANCELLATION_POLL))?;
        }
    }

    fn try_fast_path(
        self: &Arc<Self>,
        state: &mut MailboxState,
        request: &AdmissionContext<'_>,
    ) -> Option<PluginInvocationPermit> {
        let max_concurrent = request
            .export_concurrency
            .get_policy(request.export_id)
            .max_concurrent();
        let total_executing: u32 = state.executing_by_export.values().copied().sum();
        let current_executing = state
            .executing_by_export
            .get(request.export_id)
            .copied()
            .unwrap_or(0);
        if total_executing >= request.pipeline_limit
            || current_executing >= max_concurrent
            || !state.is_empty()
        {
            return None;
        }
        state
            .executing_by_export
            .insert(request.export_id.to_owned(), current_executing + 1);
        observe_plugin_mailbox(&self.plugin_id, true, 0);
        Some(self.permit(request.export_id))
    }

    fn enqueue_or_reject(
        &self,
        state: &mut MailboxState,
        request: &AdmissionContext<'_>,
    ) -> Result<u64, PluginRuntimeError> {
        let total_executing: u32 = state.executing_by_export.values().copied().sum();
        if state.is_full(request.class, request.maximum_queued) {
            state.overloaded += 1;
            observe_plugin_mailbox(&self.plugin_id, total_executing > 0, state.queued());
            return Err(busy(request.plugin_id, state));
        }
        let ticket = state.enqueue(request.class);
        observe_plugin_mailbox(&self.plugin_id, total_executing > 0, state.queued());
        Ok(ticket)
    }

    fn dispatch_if_ready(
        self: &Arc<Self>,
        state: &mut MailboxState,
        ticket: u64,
        request: &AdmissionContext<'_>,
        max_concurrent: u32,
    ) -> Option<PluginInvocationPermit> {
        if !state.can_dispatch(
            ticket,
            request.class,
            request.export_id,
            max_concurrent,
            request.pipeline_limit,
        ) {
            return None;
        }
        state.dispatch(ticket, request.class, request.export_id);
        observe_plugin_mailbox(request.plugin_id, true, state.queued());
        Some(self.permit(request.export_id))
    }

    async fn wait_async_for_turn(
        self: &Arc<Self>,
        request: AdmissionContext<'_>,
        ticket: u64,
    ) -> Result<PluginInvocationPermit, PluginRuntimeError> {
        let max_concurrent = request
            .export_concurrency
            .get_policy(request.export_id)
            .max_concurrent();
        loop {
            let remaining = match request.ensure_active() {
                Ok(remaining) => remaining,
                Err(error) => return Err(self.remove_queued(ticket, request.class, error)),
            };
            let notified = self.async_changed.notified();
            {
                let mut state = self.lock_state()?;
                if let Some(permit) =
                    self.dispatch_if_ready(&mut state, ticket, &request, max_concurrent)
                {
                    return Ok(permit);
                }
            }
            let cancellation = request.context.cancellation();
            tokio::select! {
                _ = cancellation.cancelled() => {}
                _ = tokio::time::sleep(remaining) => {}
                _ = notified => {}
            }
        }
    }
    fn wait<'state>(
        &self,
        state: MutexGuard<'state, MailboxState>,
        duration: Duration,
    ) -> Result<MutexGuard<'state, MailboxState>, PluginRuntimeError> {
        self.changed
            .wait_timeout(state, duration)
            .map(|(state, _)| state)
            .map_err(|_| PluginRuntimeError::InvocationAdmissionPoisoned)
    }

    fn remove_terminal(
        &self,
        mut state: MutexGuard<'_, MailboxState>,
        ticket: u64,
        class: PluginInvocationClass,
        error: PluginRuntimeError,
    ) -> PluginRuntimeError {
        state.queue_mut(class).retain(|queued| *queued != ticket);
        match error {
            PluginRuntimeError::InvocationCancelled { .. } => state.cancelled += 1,
            PluginRuntimeError::InvocationTimeout { .. } => state.expired += 1,
            _ => {}
        }
        observe_plugin_mailbox(
            &self.plugin_id,
            !state.executing_by_export.is_empty(),
            state.queued(),
        );
        self.changed.notify_all();
        self.async_changed.notify_waiters();
        error
    }

    fn remove_queued(
        &self,
        ticket: u64,
        class: PluginInvocationClass,
        error: PluginRuntimeError,
    ) -> PluginRuntimeError {
        match self.lock_state() {
            Ok(state) => self.remove_terminal(state, ticket, class, error),
            Err(_) => error,
        }
    }

    fn permit(self: &Arc<Self>, export_id: &str) -> PluginInvocationPermit {
        PluginInvocationPermit {
            mailbox: Arc::clone(self),
            export_id: export_id.to_string(),
        }
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, MailboxState>, PluginRuntimeError> {
        self.state
            .lock()
            .map_err(|_| PluginRuntimeError::InvocationAdmissionPoisoned)
    }
}

impl MailboxState {
    fn enqueue(&mut self, class: PluginInvocationClass) -> u64 {
        let ticket = self.next_ticket;
        self.next_ticket = self.next_ticket.wrapping_add(1);
        self.queue_mut(class).push_back(ticket);
        ticket
    }

    fn can_dispatch(
        &self,
        ticket: u64,
        class: PluginInvocationClass,
        export_id: &str,
        max_concurrent: u32,
        pipeline_limit: u32,
    ) -> bool {
        let total_executing: u32 = self.executing_by_export.values().copied().sum();
        if total_executing >= pipeline_limit {
            return false;
        }
        let current_executing = self
            .executing_by_export
            .get(export_id)
            .copied()
            .unwrap_or(0);
        if current_executing >= max_concurrent {
            return false;
        }
        match class {
            PluginInvocationClass::Foreground => self.foreground_queue.front() == Some(&ticket),
            PluginInvocationClass::Background => {
                self.foreground_queue.is_empty() && self.background_queue.front() == Some(&ticket)
            }
        }
    }

    fn dispatch(&mut self, ticket: u64, class: PluginInvocationClass, export_id: &str) {
        // Dequeue in release builds too; an assertion must never own a mutation.
        let dispatched = self.queue_mut(class).pop_front();
        debug_assert_eq!(dispatched, Some(ticket));
        let current = self
            .executing_by_export
            .get(export_id)
            .copied()
            .unwrap_or(0);
        self.executing_by_export
            .insert(export_id.to_string(), current + 1);
    }

    fn is_empty(&self) -> bool {
        self.foreground_queue.is_empty() && self.background_queue.is_empty()
    }

    fn queued(&self) -> usize {
        self.foreground_queue.len() + self.background_queue.len()
    }

    fn is_full(&self, class: PluginInvocationClass, maximum_queued: usize) -> bool {
        if self.queued() >= maximum_queued {
            return true;
        }
        class == PluginInvocationClass::Background
            && self.background_queue.len() >= maximum_queued / 2
    }

    fn queue_mut(&mut self, class: PluginInvocationClass) -> &mut VecDeque<u64> {
        match class {
            PluginInvocationClass::Foreground => &mut self.foreground_queue,
            PluginInvocationClass::Background => &mut self.background_queue,
        }
    }

    fn snapshot(&self, plugin_id: &str) -> PluginAdmissionSnapshot {
        let executing = !self.executing_by_export.is_empty();
        PluginAdmissionSnapshot {
            plugin_id: plugin_id.to_owned(),
            executing,
            queued: self.queued(),
            foreground_queued: self.foreground_queue.len(),
            background_queued: self.background_queue.len(),
            cancelled: self.cancelled,
            expired: self.expired,
            overloaded: self.overloaded,
        }
    }
}

impl Drop for PluginInvocationPermit {
    fn drop(&mut self) {
        if let Ok(mut state) = self.mailbox.state.lock() {
            let current = state
                .executing_by_export
                .get(self.export_id.as_str())
                .copied()
                .unwrap_or(0);
            if current > 1 {
                state
                    .executing_by_export
                    .insert(self.export_id.clone(), current - 1);
            } else {
                state.executing_by_export.remove(self.export_id.as_str());
            }
            let still_executing = !state.executing_by_export.is_empty();
            observe_plugin_mailbox(&self.mailbox.plugin_id, still_executing, state.queued());
            self.mailbox.changed.notify_all();
            self.mailbox.async_changed.notify_waiters();
        }
    }
}

/// Pushes the per-plugin mailbox depth/executing gauges into the metrics recorder.
fn observe_plugin_mailbox(plugin_id: &str, executing: bool, queued: usize) {
    metrics::gauge!("lumvise_plugin_invocation_queued", "plugin" => plugin_id.to_owned())
        .set(queued as f64);
    metrics::gauge!("lumvise_plugin_invocation_executing", "plugin" => plugin_id.to_owned())
        .set(if executing { 1.0 } else { 0.0 });
}

fn busy(plugin_id: &str, state: &MailboxState) -> PluginRuntimeError {
    let total_executing: u32 = state.executing_by_export.values().copied().sum();
    PluginRuntimeError::InvocationBusy {
        plugin_id: plugin_id.to_owned(),
        active: total_executing as usize,
        queued: state.queued(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Instant;

    use super::*;
    use crate::PluginInvocationClass;
    use crate::system::export_concurrency_policy::ExportConcurrencyPolicy;

    #[test]
    fn mailbox_is_fifo_and_reports_execution_without_lifecycle_state() {
        let admission = Arc::new(PluginInvocationAdmission::new(4, 1));
        let first = context("first", Duration::from_secs(1));
        let registry = ExportConcurrencyRegistry::new();
        let permit = admission
            .admit(
                "plugin-a",
                "export-id",
                &first,
                &registry,
                Duration::from_secs(1),
            )
            .expect("first admitted");
        let (order_tx, order_rx) = mpsc::channel();
        let first_waiter = spawn_waiter(Arc::clone(&admission), "second", 1, order_tx.clone());
        wait_for_queue(&admission, 1);
        let second_waiter = spawn_waiter(Arc::clone(&admission), "third", 2, order_tx);
        wait_for_queue(&admission, 2);

        drop(permit);

        assert_eq!(order_rx.recv().expect("first order"), 1);
        assert_eq!(order_rx.recv().expect("second order"), 2);
        first_waiter.join().expect("first waiter");
        second_waiter.join().expect("second waiter");
        assert_eq!(admission.snapshot("plugin-a").unwrap().queued, 0);
    }

    #[test]
    fn full_mailbox_rejects_and_cancelled_waiter_is_removed() {
        let admission = Arc::new(PluginInvocationAdmission::new(1, 1));
        let active = context("active", Duration::from_secs(1));
        let registry = ExportConcurrencyRegistry::new();
        let permit = admission
            .admit(
                "plugin-a",
                "export-id",
                &active,
                &registry,
                Duration::from_secs(1),
            )
            .expect("active admitted");
        let queued = context("queued", Duration::from_secs(1));
        let cancellation = queued.cancellation();
        let waiting_admission = Arc::clone(&admission);
        let waiter = thread::spawn(move || {
            let registry = ExportConcurrencyRegistry::new();
            waiting_admission.admit(
                "plugin-a",
                "export-id",
                &queued,
                &registry,
                Duration::from_secs(1),
            )
        });
        wait_for_queue(&admission, 1);

        let overloaded = context("overloaded", Duration::from_secs(1));
        let error = admission
            .admit(
                "plugin-a",
                "export-id",
                &overloaded,
                &registry,
                Duration::from_secs(1),
            )
            .err()
            .expect("mailbox full");
        cancellation.cancel();
        let cancelled = waiter
            .join()
            .expect("waiter join")
            .err()
            .expect("cancelled");
        drop(permit);

        assert!(matches!(error, PluginRuntimeError::InvocationBusy { .. }));
        assert!(matches!(
            cancelled,
            PluginRuntimeError::InvocationCancelled { .. }
        ));
        let snapshot = admission.snapshot("plugin-a").unwrap();
        assert_eq!(
            (snapshot.queued, snapshot.cancelled, snapshot.overloaded),
            (0, 1, 1)
        );
    }

    #[test]
    fn queued_deadline_expires_and_releases_fifo_position() {
        let admission = Arc::new(PluginInvocationAdmission::new(2, 1));
        let active = context("active", Duration::from_secs(1));
        let registry = ExportConcurrencyRegistry::new();
        let _permit = admission
            .admit(
                "plugin-a",
                "export-id",
                &active,
                &registry,
                Duration::from_secs(1),
            )
            .expect("active admitted");
        let expiring = context("expiring", Duration::from_millis(20));

        let error = admission
            .admit(
                "plugin-a",
                "export-id",
                &expiring,
                &registry,
                Duration::from_secs(1),
            )
            .err()
            .expect("queue deadline");

        assert!(matches!(
            error,
            PluginRuntimeError::InvocationTimeout { .. }
        ));
        let snapshot = admission.snapshot("plugin-a").unwrap();
        assert_eq!((snapshot.queued, snapshot.expired), (0, 1));
    }

    #[test]
    fn foreground_capacity_and_dispatch_are_reserved_from_background_work() {
        let admission = Arc::new(PluginInvocationAdmission::new(4, 1));
        let active = context("active", Duration::from_secs(1));
        let registry = ExportConcurrencyRegistry::new();
        let permit = admission
            .admit(
                "plugin-a",
                "export-id",
                &active,
                &registry,
                Duration::from_secs(1),
            )
            .expect("active admitted");
        let (order_tx, order_rx) = mpsc::channel();
        let background_one = spawn_class_waiter(
            Arc::clone(&admission),
            "background-1",
            PluginInvocationClass::Background,
            1,
            order_tx.clone(),
        );
        wait_for_queue(&admission, 1);
        let background_two = spawn_class_waiter(
            Arc::clone(&admission),
            "background-2",
            PluginInvocationClass::Background,
            2,
            order_tx.clone(),
        );
        wait_for_queue(&admission, 2);

        let rejected = class_context(
            "background-3",
            PluginInvocationClass::Background,
            Duration::from_secs(1),
        );
        assert!(matches!(
            admission.admit(
                "plugin-a",
                "export-id",
                &rejected,
                &registry,
                Duration::from_secs(1)
            ),
            Err(PluginRuntimeError::InvocationBusy { .. })
        ));
        let foreground = spawn_class_waiter(
            Arc::clone(&admission),
            "foreground",
            PluginInvocationClass::Foreground,
            3,
            order_tx,
        );
        wait_for_queue(&admission, 3);
        let snapshot = admission.snapshot("plugin-a").unwrap();
        assert_eq!(
            (snapshot.foreground_queued, snapshot.background_queued),
            (1, 2)
        );

        drop(permit);

        assert_eq!(order_rx.recv().expect("foreground order"), 3);
        assert_eq!(order_rx.recv().expect("first background order"), 1);
        assert_eq!(order_rx.recv().expect("second background order"), 2);
        foreground.join().expect("foreground waiter");
        background_one.join().expect("first background waiter");
        background_two.join().expect("second background waiter");
    }

    #[test]
    fn pipeline_limit_allows_concurrent_invocations() {
        let admission = Arc::new(PluginInvocationAdmission::new(4, 2));
        let mut registry = ExportConcurrencyRegistry::new();
        registry.set_policy(
            "export-id".to_string(),
            ExportConcurrencyPolicy::Parallel(2),
        );
        let first = context("first", Duration::from_secs(1));
        let first_permit = admission
            .admit(
                "plugin-a",
                "export-id",
                &first,
                &registry,
                Duration::from_secs(1),
            )
            .expect("first admitted");
        let second = context("second", Duration::from_secs(1));
        let second_permit = admission
            .admit(
                "plugin-a",
                "export-id",
                &second,
                &registry,
                Duration::from_secs(1),
            )
            .expect("second admitted concurrently");

        let snapshot = admission.snapshot("plugin-a").unwrap();
        assert!(snapshot.executing);
        assert_eq!(snapshot.queued, 0);

        drop(first_permit);
        drop(second_permit);
        assert_eq!(admission.snapshot("plugin-a").unwrap().queued, 0);
    }

    #[test]
    fn pipeline_limit_respects_configured_capacity() {
        let admission = Arc::new(PluginInvocationAdmission::new(4, 2));
        let mut registry = ExportConcurrencyRegistry::new();
        registry.set_policy(
            "export-id".to_string(),
            ExportConcurrencyPolicy::Parallel(2),
        );
        let first = admission
            .admit(
                "plugin-a",
                "export-id",
                &context("first", Duration::from_secs(5)),
                &registry,
                Duration::from_secs(5),
            )
            .expect("first admitted");
        let second = admission
            .admit(
                "plugin-a",
                "export-id",
                &context("second", Duration::from_secs(5)),
                &registry,
                Duration::from_secs(5),
            )
            .expect("second admitted");

        let snapshot = admission.snapshot("plugin-a").unwrap();
        assert!(snapshot.executing);
        assert_eq!(snapshot.queued, 0);

        let (order_tx, order_rx) = mpsc::channel();
        let waiting_admission = Arc::clone(&admission);
        let mut waiting_registry = ExportConcurrencyRegistry::new();
        waiting_registry.set_policy(
            "export-id".to_string(),
            ExportConcurrencyPolicy::Parallel(2),
        );
        let third = thread::spawn(move || {
            let context = class_context(
                "third",
                PluginInvocationClass::Foreground,
                Duration::from_secs(5),
            );
            let _permit = waiting_admission
                .admit(
                    "plugin-a",
                    "export-id",
                    &context,
                    &waiting_registry,
                    Duration::from_secs(5),
                )
                .expect("third admitted");
            order_tx.send(1).expect("send order");
        });
        wait_for_queue(&admission, 1);

        admission.snapshot("plugin-a").unwrap();
        drop(first);
        assert_eq!(order_rx.recv().expect("third order"), 1);
        third.join().expect("third waiter");
        drop(second);
    }

    fn context(request_id: &str, duration: Duration) -> PluginInvocationContext {
        class_context(request_id, PluginInvocationClass::Foreground, duration)
    }

    fn class_context(
        request_id: &str,
        class: PluginInvocationClass,
        duration: Duration,
    ) -> PluginInvocationContext {
        PluginInvocationContext::new(request_id, "test-owner", class, Instant::now() + duration)
    }

    fn spawn_waiter(
        admission: Arc<PluginInvocationAdmission>,
        request_id: &'static str,
        order: u8,
        sender: mpsc::Sender<u8>,
    ) -> thread::JoinHandle<()> {
        spawn_class_waiter(
            admission,
            request_id,
            PluginInvocationClass::Foreground,
            order,
            sender,
        )
    }

    fn spawn_class_waiter(
        admission: Arc<PluginInvocationAdmission>,
        request_id: &'static str,
        class: PluginInvocationClass,
        order: u8,
        sender: mpsc::Sender<u8>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let context = class_context(request_id, class, Duration::from_secs(1));
            let registry = ExportConcurrencyRegistry::new();
            let _permit = admission
                .admit(
                    "plugin-a",
                    "export-id",
                    &context,
                    &registry,
                    Duration::from_secs(1),
                )
                .expect("waiter admitted");
            sender.send(order).expect("send order");
        })
    }

    fn wait_for_queue(admission: &PluginInvocationAdmission, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while admission.snapshot("plugin-a").unwrap().queued != expected {
            assert!(Instant::now() < deadline, "queue did not reach {expected}");
            thread::yield_now();
        }
    }
}
