//! FIFO per-session turn mailbox for native-LLM handoff sessions.
//!
//! `runtime.turn_wait` is the rendezvous between the Assistant plugin's
//! `advance()` handler (push side, `notify`) and a declared native-LLM
//! caller's segmented long-poll (`await`). It mirrors the proven bounded-wait
//! design of `background_jobs.rs`, keyed by session instead of job, with an
//! externally supplied payload instead of an executor thread.
//!
//! Unlike a single-slot mailbox, a second utterance arriving before the
//! caller re-issues `await` is queued in FIFO order rather than dropped —
//! the caller's own round-trip latency (model inference + tool call) must
//! never lose user speech.

use lumvise_plugin_runtime::HostCapabilityError;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use super::{TURN_WAIT, exact_fields, exact_object, failed, invalid, required, required_string};

// Same ceiling as background_jobs::MAX_WAIT_MS: bounded long-poll segments
// must fit under the unconditional 60-second plugin-invocation deadline.
const TURN_WAIT_MAX_MS: u64 = 55_000;

type SessionKey = (String, String);

pub(super) struct TurnWaitAdapter {
    slots: Mutex<HashMap<SessionKey, TurnWaitMailbox>>,
    ready: Condvar,
}

/// One session's mailbox: a FIFO of turns not yet delivered, the
/// `turn_id` of the last delivered turn (for dedupe against turns already
/// consumed), and the terminal reason once the session has ended.
#[derive(Clone, Default)]
struct TurnWaitMailbox {
    pending: VecDeque<Value>,
    last_delivered_turn_id: Option<String>,
    ended: Option<Value>,
    waiters: u32,
}

impl TurnWaitAdapter {
    pub(super) fn new() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            ready: Condvar::new(),
        }
    }

    pub(super) fn invoke(
        &self,
        plugin_id: &str,
        input: Value,
    ) -> Result<Value, HostCapabilityError> {
        let fields = exact_object(TURN_WAIT, &input)?;
        match required_string(TURN_WAIT, fields, "operation")? {
            "await" => self.await_turn(plugin_id, fields),
            "notify" => self.notify(plugin_id, fields),
            "session_ended" => self.session_ended(plugin_id, fields),
            "cancel" => self.cancel(plugin_id, fields),
            other => Err(invalid(
                TURN_WAIT,
                &json!(other),
                "operation await, notify, session_ended, or cancel",
            )),
        }
    }

    fn await_turn(
        &self,
        plugin_id: &str,
        fields: &Map<String, Value>,
    ) -> Result<Value, HostCapabilityError> {
        exact_fields(TURN_WAIT, fields, &["operation", "session_id", "wait_ms"])?;
        let key = session_key(plugin_id, fields)?;
        let wait_ms = required_wait_ms(fields)?;
        let mut slots = self.lock_slots()?;
        if let Some(delivered) = try_take(&mut slots, &key) {
            return Ok(delivered);
        }
        slots.entry(key.clone()).or_default().waiters += 1;
        let (mut slots, _) = self
            .ready
            .wait_timeout_while(slots, Duration::from_millis(wait_ms), |slots| {
                slots
                    .get(&key)
                    .is_some_and(|mailbox| mailbox.pending.is_empty() && mailbox.ended.is_none())
            })
            .map_err(|_| failed(TURN_WAIT, "turn wait mutex poisoned"))?;
        if let Some(mailbox) = slots.get_mut(&key) {
            mailbox.waiters = mailbox.waiters.saturating_sub(1);
        }
        Ok(try_take(&mut slots, &key).unwrap_or_else(|| json!({"status": "timeout"})))
    }

    fn notify(
        &self,
        plugin_id: &str,
        fields: &Map<String, Value>,
    ) -> Result<Value, HostCapabilityError> {
        exact_fields(TURN_WAIT, fields, &["operation", "session_id", "turn"])?;
        let key = session_key(plugin_id, fields)?;
        let turn = required(fields, "turn")?.clone();
        let mut slots = self.lock_slots()?;
        let mailbox = slots.entry(key).or_default();
        if mailbox.ended.is_some() {
            // A turn arriving after the session ended must not resurrect it.
            return Ok(json!({"status": "ended"}));
        }
        let turn_id = turn_id_of(&turn);
        let duplicate = turn_id.is_some()
            && (mailbox.last_delivered_turn_id == turn_id
                || mailbox
                    .pending
                    .iter()
                    .any(|queued| turn_id_of(queued) == turn_id));
        if duplicate {
            return Ok(json!({"status": "duplicate"}));
        }
        mailbox.pending.push_back(turn);
        self.ready.notify_all();
        Ok(json!({"status": "ok"}))
    }

    fn session_ended(
        &self,
        plugin_id: &str,
        fields: &Map<String, Value>,
    ) -> Result<Value, HostCapabilityError> {
        exact_fields(TURN_WAIT, fields, &["operation", "session_id", "reason"])?;
        let key = session_key(plugin_id, fields)?;
        let reason = required(fields, "reason")?.clone();
        self.end(&key, reason)
    }

    fn cancel(
        &self,
        plugin_id: &str,
        fields: &Map<String, Value>,
    ) -> Result<Value, HostCapabilityError> {
        exact_fields(TURN_WAIT, fields, &["operation", "session_id"])?;
        let key = session_key(plugin_id, fields)?;
        self.end(&key, json!("cancelled"))
    }

    fn end(&self, key: &SessionKey, reason: Value) -> Result<Value, HostCapabilityError> {
        let mut slots = self.lock_slots()?;
        let mailbox = slots.entry(key.clone()).or_default();
        mailbox.ended = Some(reason);
        // No one is parked and nothing is queued: nothing will ever call
        // `await` again to trigger eviction, so evict now rather than
        // leaking this session's slot for the rest of the process.
        if mailbox.waiters == 0 && mailbox.pending.is_empty() {
            slots.remove(key);
        }
        self.ready.notify_all();
        Ok(json!({"status": "ok"}))
    }

    fn lock_slots(
        &self,
    ) -> Result<MutexGuard<'_, HashMap<SessionKey, TurnWaitMailbox>>, HostCapabilityError> {
        self.slots
            .lock()
            .map_err(|_| failed(TURN_WAIT, "turn wait mutex poisoned"))
    }

    #[cfg(test)]
    fn slot_is_waiting(&self, plugin_id: &str, session_id: &str) -> bool {
        let key = (plugin_id.to_owned(), session_id.to_owned());
        self.slots
            .lock()
            .is_ok_and(|slots| slots.get(&key).is_some_and(|mailbox| mailbox.waiters > 0))
    }

    #[cfg(test)]
    fn mailbox_count(&self) -> usize {
        self.slots.lock().map(|slots| slots.len()).unwrap_or(0)
    }
}

/// Delivers the oldest queued turn, or the terminal reason once the mailbox
/// has both ended and drained — evicting the session's slot at that point so
/// `Ended` mailboxes never outlive their one delivery.
fn try_take(slots: &mut HashMap<SessionKey, TurnWaitMailbox>, key: &SessionKey) -> Option<Value> {
    let mailbox = slots.get_mut(key)?;
    if let Some(turn) = mailbox.pending.pop_front() {
        if let Some(turn_id) = turn_id_of(&turn) {
            mailbox.last_delivered_turn_id = Some(turn_id);
        }
        return Some(json!({"status": "ready", "turn": turn}));
    }
    if let Some(reason) = mailbox.ended.clone() {
        slots.remove(key);
        return Some(json!({"status": "ended", "turn": {"reason": reason}}));
    }
    None
}

fn turn_id_of(turn: &Value) -> Option<String> {
    turn.get("turn_id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn session_key(
    plugin_id: &str,
    fields: &Map<String, Value>,
) -> Result<SessionKey, HostCapabilityError> {
    let session_id = required_string(TURN_WAIT, fields, "session_id")?;
    Ok((plugin_id.to_owned(), session_id.to_owned()))
}

fn required_wait_ms(fields: &Map<String, Value>) -> Result<u64, HostCapabilityError> {
    let value = required(fields, "wait_ms")?;
    value
        .as_u64()
        .filter(|wait| (1..=TURN_WAIT_MAX_MS).contains(wait))
        .ok_or_else(|| invalid(TURN_WAIT, value, "wait_ms integer from 1 through 55000"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn two_rapid_notifications_are_both_queued_and_delivered_in_order() {
        let adapter = TurnWaitAdapter::new();
        adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "notify", "session_id": "session-a",
                    "turn": {"transcript": "first", "turn_id": "t-1"}}),
            )
            .expect("first notify queues");
        adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "notify", "session_id": "session-a",
                    "turn": {"transcript": "second", "turn_id": "t-2"}}),
            )
            .expect("second notify queues instead of overwriting the first");

        let first = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "await", "session_id": "session-a", "wait_ms": 1}),
            )
            .expect("first await");
        assert_eq!(first["turn"]["transcript"], "first");

        let second = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "await", "session_id": "session-a", "wait_ms": 1}),
            )
            .expect("second await");
        assert_eq!(second["turn"]["transcript"], "second");
    }

    #[test]
    fn concurrent_awaiters_each_receive_one_queued_turn() {
        let adapter = TurnWaitAdapter::new();
        std::thread::scope(|scope| {
            let parked = scope.spawn(|| {
                adapter.invoke(
                    "plugin.owner",
                    json!({"operation": "await", "session_id": "session-a", "wait_ms": 5_000}),
                )
            });
            while !adapter.slot_is_waiting("plugin.owner", "session-a") {
                std::thread::yield_now();
            }
            adapter
                .invoke(
                    "plugin.owner",
                    json!({"operation": "notify", "session_id": "session-a",
                        "turn": {"transcript": "hello", "turn_id": "t-1"}}),
                )
                .expect("notify parked waiter");
            let delivered = parked
                .join()
                .expect("parked await joined")
                .expect("parked await");
            assert_eq!(delivered["status"], "ready");
            assert_eq!(delivered["turn"]["transcript"], "hello");
        });
    }

    #[test]
    fn notify_before_any_waiter_buffers_for_the_next_await() {
        let adapter = TurnWaitAdapter::new();
        adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "notify", "session_id": "session-a",
                    "turn": {"transcript": "buffered", "turn_id": "t-1"}}),
            )
            .expect("buffered notify");

        let result = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "await", "session_id": "session-a", "wait_ms": 1}),
            )
            .expect("await consumes buffered turn");

        assert_eq!(result["status"], "ready");
        assert_eq!(result["turn"]["transcript"], "buffered");
    }

    #[test]
    fn delivered_turn_is_not_replayed_by_a_duplicate_notification() {
        let adapter = TurnWaitAdapter::new();
        let opening = json!({"transcript": "debug duplicate delivery", "turn_id": "session-a:1:0"});
        adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "notify", "session_id": "session-a", "turn": opening}),
            )
            .expect("opening notify");
        let first = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "await", "session_id": "session-a", "wait_ms": 1}),
            )
            .expect("opening await");
        assert_eq!(first["status"], "ready");
        assert_eq!(first["turn"]["transcript"], "debug duplicate delivery");

        adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "notify", "session_id": "session-a",
                    "turn": {"transcript": "debug duplicate delivery", "turn_id": "session-a:1:0"}}),
            )
            .expect("duplicate notify");
        let duplicate = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "await", "session_id": "session-a", "wait_ms": 1}),
            )
            .expect("await after duplicate notify");
        assert_eq!(duplicate["status"], "timeout");

        adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "notify", "session_id": "session-a",
                    "turn": {"transcript": "later user turn", "turn_id": "session-a:1:1"}}),
            )
            .expect("later notify");
        let later = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "await", "session_id": "session-a", "wait_ms": 1}),
            )
            .expect("later await");
        assert_eq!(later["status"], "ready");
        assert_eq!(later["turn"]["transcript"], "later user turn");
    }

    #[test]
    fn duplicate_notify_is_rejected_even_while_still_queued() {
        let adapter = TurnWaitAdapter::new();
        adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "notify", "session_id": "session-a",
                    "turn": {"transcript": "queued", "turn_id": "t-1"}}),
            )
            .expect("first notify queues");
        let duplicate = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "notify", "session_id": "session-a",
                    "turn": {"transcript": "queued again", "turn_id": "t-1"}}),
            )
            .expect("duplicate notify is not an error");
        assert_eq!(duplicate["status"], "duplicate");

        let delivered = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "await", "session_id": "session-a", "wait_ms": 1}),
            )
            .expect("await delivers only the original");
        assert_eq!(delivered["turn"]["transcript"], "queued");
        let empty = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "await", "session_id": "session-a", "wait_ms": 1}),
            )
            .expect("no second turn was queued");
        assert_eq!(empty["status"], "timeout");
    }

    #[test]
    fn elapsed_await_returns_timeout_and_leaves_the_slot_reusable() {
        let adapter = TurnWaitAdapter::new();
        let elapsed = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "await", "session_id": "session-a", "wait_ms": 1}),
            )
            .expect("bounded await");
        assert_eq!(elapsed, json!({"status": "timeout"}));

        adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "notify", "session_id": "session-a",
                    "turn": {"transcript": "after timeout", "turn_id": "t-2"}}),
            )
            .expect("notify after timeout");
        let result = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "await", "session_id": "session-a", "wait_ms": 1}),
            )
            .expect("slot reusable after timeout");
        assert_eq!(result["turn"]["transcript"], "after timeout");
    }

    #[test]
    fn session_ended_wakes_a_parked_waiter_with_the_reason() {
        let adapter = TurnWaitAdapter::new();
        std::thread::scope(|scope| {
            let parked = scope.spawn(|| {
                adapter.invoke(
                    "plugin.owner",
                    json!({"operation": "await", "session_id": "session-a", "wait_ms": 5_000}),
                )
            });
            while !adapter.slot_is_waiting("plugin.owner", "session-a") {
                std::thread::yield_now();
            }
            adapter
                .invoke(
                    "plugin.owner",
                    json!({"operation": "session_ended", "session_id": "session-a",
                        "reason": "Completed"}),
                )
                .expect("end session");
            let result = parked
                .join()
                .expect("parked await joined")
                .expect("ended await");
            assert_eq!(result["status"], "ended");
            assert_eq!(result["turn"]["reason"], "Completed");
        });
    }

    #[test]
    fn ended_session_with_no_waiter_is_evicted_immediately() {
        let adapter = TurnWaitAdapter::new();
        adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "session_ended", "session_id": "session-a",
                    "reason": "Completed"}),
            )
            .expect("end session with nobody parked");
        assert_eq!(adapter.mailbox_count(), 0);
    }

    #[test]
    fn mailbox_preserves_user_turns_when_caller_is_delayed() {
        let adapter = TurnWaitAdapter::new();
        for index in 0..32 {
            adapter
                .invoke(
                    "plugin.owner",
                    json!({"operation": "notify", "session_id": "session-a",
                        "turn": {"transcript": format!("turn-{index}"), "turn_id": format!("t-{index}")}}),
                )
                .expect("notify accepted");
        }
        let first_delivered = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "await", "session_id": "session-a", "wait_ms": 1}),
            )
            .expect("await delivers oldest surviving turn");
        assert_eq!(first_delivered["turn"]["transcript"], "turn-0");
        for index in 1..32 {
            let delivered = adapter
                .invoke(
                    "plugin.owner",
                    json!({"operation":"await",
                "session_id":"session-a", "wait_ms":1}),
                )
                .expect("queued turn");
            assert_eq!(delivered["turn"]["transcript"], format!("turn-{index}"));
        }
    }
}
