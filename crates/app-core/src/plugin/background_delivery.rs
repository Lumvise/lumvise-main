//! Durable scheduling and delivery for generic compiled background exports.

use std::{
    collections::HashMap,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use crate::PluginInvocationStatus;
use chrono::Utc;
use lumvise_db_core::{
    PluginBackgroundDelivery, PluginBackgroundRegistrationSpec, RelationalOperation,
    RelationalPersistence, RelationalResult,
};
use lumvise_plugin_package::BackgroundDeliveryPolicy;
use lumvise_plugin_protocol::WireOutcome;
use lumvise_plugin_runtime::{BackgroundExportKind, PublishedBackgroundExport};
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};

use crate::{AppCoreError, PluginEndpoints, Result};

const MAX_DELIVERIES_PER_TICK: usize = 1_000;
const MAX_BACKGROUND_EXPORTS: usize = 256;
const BACKGROUND_INVOCATION_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) struct BackgroundDeliveryRun {
    pub delivery: PluginBackgroundDelivery,
    pub status: PluginInvocationStatus,
    pub output: Value,
}

struct ClaimedBackgroundDelivery {
    ordinal: usize,
    delivery: PluginBackgroundDelivery,
    policy: BackgroundDeliveryPolicy,
}

impl PluginEndpoints<'_> {
    pub(crate) fn sync_background_catalog(
        &self,
        now_unix_seconds: i64,
    ) -> Result<Vec<PublishedBackgroundExport>> {
        let exports = self.app.plugin_system().background_exports()?;
        sync_background_registrations(self.app.relational.as_ref(), &exports, now_unix_seconds)?;
        Ok(exports)
    }

    pub(super) fn enqueue_due_recurring(
        &self,
        exports: &[PublishedBackgroundExport],
        now_unix_seconds: i64,
    ) -> Result<()> {
        let registrations = match self.app.relational.execute(
            RelationalOperation::ListBackgroundRegistrations,
            &InvocationControl::sixty_seconds(),
        )? {
            RelationalResult::BackgroundRegistrations(registrations) => registrations,
            other => return Err(background_result_error("list registrations", other)),
        }
        .into_iter()
        .map(|registration| {
            (
                (registration.plugin_id, registration.export_id),
                registration.next_due_at,
            )
        })
        .collect::<HashMap<_, _>>();
        for export in exports {
            let BackgroundExportKind::RecurringTask {
                interval_seconds, ..
            } = export.kind
            else {
                continue;
            };
            let key = (export.plugin_id.clone(), export.export_id.clone());
            let Some(scheduled_at) = registrations.get(&key).copied().flatten() else {
                continue;
            };
            if scheduled_at > now_unix_seconds {
                continue;
            }
            let delivery_id = recurring_delivery_id(export, scheduled_at);
            let payload = json!({
                "delivery_id": delivery_id,
                "scheduled_at_unix_seconds": scheduled_at,
            });
            let _ = self.app.relational.execute(
                RelationalOperation::EnqueueRecurring {
                    plugin_id: export.plugin_id.clone(),
                    export_id: export.export_id.clone(),
                    scheduled_at,
                    now_unix_seconds,
                    interval_seconds,
                    payload,
                },
                &InvocationControl::sixty_seconds(),
            )?;
        }
        Ok(())
    }

    pub(super) fn process_due_background_kind(
        &self,
        exports: &[PublishedBackgroundExport],
        now_unix_seconds: i64,
        delivery_kind: &str,
    ) -> Result<Vec<BackgroundDeliveryRun>> {
        let policies = export_policies(exports);
        let due = match self.app.relational.execute(
            RelationalOperation::DueDeliveriesByKind {
                delivery_kind: delivery_kind.to_string(),
                now_unix_seconds,
                limit: MAX_DELIVERIES_PER_TICK,
            },
            &InvocationControl::sixty_seconds(),
        )? {
            RelationalResult::BackgroundDeliveries(deliveries) => deliveries,
            other => return Err(background_result_error("due background deliveries", other)),
        };
        let claimed = self.claim_due_deliveries(due, &policies, now_unix_seconds)?;
        self.invoke_plugin_delivery_batches(claimed, now_unix_seconds)
    }

    fn claim_due_deliveries(
        &self,
        due: Vec<PluginBackgroundDelivery>,
        policies: &HashMap<(String, String), BackgroundDeliveryPolicy>,
        now_unix_seconds: i64,
    ) -> Result<Vec<ClaimedBackgroundDelivery>> {
        let mut claimed_deliveries = Vec::new();
        for (ordinal, delivery) in due.into_iter().enumerate() {
            let key = (delivery.plugin_id.clone(), delivery.export_id.clone());
            let Some(policy) = policies.get(&key) else {
                continue;
            };
            let lease_seconds = invocation_lease_seconds(BACKGROUND_INVOCATION_TIMEOUT);
            let claimed = match self.app.relational.execute(
                RelationalOperation::ClaimDelivery {
                    delivery_id: delivery.delivery_id.clone(),
                    now_unix_seconds,
                    lease_seconds,
                },
                &InvocationControl::sixty_seconds(),
            )? {
                RelationalResult::BackgroundDelivery(delivery) => delivery,
                other => return Err(background_result_error("claim background delivery", other)),
            };
            let Some(claimed) = claimed else {
                continue;
            };
            claimed_deliveries.push(ClaimedBackgroundDelivery {
                ordinal,
                delivery: claimed,
                policy: policy.clone(),
            });
        }
        Ok(claimed_deliveries)
    }

    fn invoke_plugin_delivery_batches(
        &self,
        claimed: Vec<ClaimedBackgroundDelivery>,
        now_unix_seconds: i64,
    ) -> Result<Vec<BackgroundDeliveryRun>> {
        let mut by_plugin: HashMap<String, Vec<ClaimedBackgroundDelivery>> = HashMap::new();
        for delivery in claimed {
            by_plugin
                .entry(delivery.delivery.plugin_id.clone())
                .or_default()
                .push(delivery);
        }
        let mut outcomes = thread::scope(|scope| {
            let (sender, receiver) = mpsc::channel();
            for deliveries in by_plugin.into_values() {
                let sender = sender.clone();
                scope.spawn(move || {
                    self.invoke_plugin_delivery_batch(deliveries, now_unix_seconds, sender)
                });
            }
            drop(sender);
            receiver.into_iter().collect::<Vec<_>>()
        });
        outcomes.sort_by_key(|(ordinal, _)| *ordinal);
        outcomes.into_iter().map(|(_, outcome)| outcome).collect()
    }

    fn invoke_plugin_delivery_batch(
        &self,
        deliveries: Vec<ClaimedBackgroundDelivery>,
        now_unix_seconds: i64,
        sender: mpsc::Sender<(usize, Result<BackgroundDeliveryRun>)>,
    ) {
        for claimed in deliveries {
            let outcome = self.invoke_background_delivery(
                claimed.delivery,
                &claimed.policy,
                now_unix_seconds,
            );
            if sender.send((claimed.ordinal, outcome)).is_err() {
                return;
            }
        }
    }

    fn invoke_background_delivery(
        &self,
        delivery: PluginBackgroundDelivery,
        policy: &BackgroundDeliveryPolicy,
        now_unix_seconds: i64,
    ) -> Result<BackgroundDeliveryRun> {
        let timeout = BACKGROUND_INVOCATION_TIMEOUT;
        let request = super::invocation::background_invocation_request(
            &delivery.plugin_id,
            &delivery.export_id,
            delivery.payload.clone(),
            &delivery.delivery_id,
            Instant::now() + timeout,
        );
        let outcome = self.app.plugin_system().invoke_controlled(request);
        match outcome {
            Ok(WireOutcome::Succeeded { value }) => {
                let result = self.app.relational.execute(
                    RelationalOperation::CompleteDelivery {
                        delivery_id: delivery.delivery_id.clone(),
                    },
                    &InvocationControl::sixty_seconds(),
                )?;
                if !matches!(
                    result,
                    RelationalResult::DeliveryTransition { changed: true }
                ) {
                    return Err(background_result_error(
                        "complete background delivery",
                        result,
                    ));
                }
                Ok(BackgroundDeliveryRun {
                    delivery,
                    status: PluginInvocationStatus::Completed,
                    output: value,
                })
            }
            Ok(WireOutcome::Failed { error }) => {
                let output = json!({"error": {
                    "code": error.code,
                    "message": error.message,
                    "details": error.details,
                    "retryable": error.retryable,
                }});
                self.record_delivery_failure(
                    &delivery,
                    policy,
                    now_unix_seconds,
                    &output.to_string(),
                    error.retryable,
                )?;
                Ok(failed_run(delivery, output))
            }
            Err(error) => {
                self.record_delivery_failure(
                    &delivery,
                    policy,
                    now_unix_seconds,
                    &error.to_string(),
                    error.retryable(),
                )?;
                Ok(failed_run(delivery, json!({"error": error.to_string()})))
            }
        }
    }

    fn record_delivery_failure(
        &self,
        delivery: &PluginBackgroundDelivery,
        policy: &BackgroundDeliveryPolicy,
        now_unix_seconds: i64,
        error: &str,
        retryable: bool,
    ) -> Result<()> {
        if retryable && delivery.attempts < policy.max_attempts {
            let next =
                now_unix_seconds.saturating_add(retry_delay_seconds(policy, delivery.attempts));
            let result = self.app.relational.execute(
                RelationalOperation::RetryDelivery {
                    delivery_id: delivery.delivery_id.clone(),
                    next_attempt_at: next,
                    error: error.to_string(),
                },
                &InvocationControl::sixty_seconds(),
            )?;
            if !matches!(
                result,
                RelationalResult::DeliveryTransition { changed: true }
            ) {
                return Err(background_result_error("retry background delivery", result));
            }
            return Ok(());
        }
        let result = self.app.relational.execute(
            RelationalOperation::DeadLetterDelivery {
                delivery_id: delivery.delivery_id.clone(),
                error: error.to_string(),
            },
            &InvocationControl::sixty_seconds(),
        )?;
        if !matches!(
            result,
            RelationalResult::DeliveryTransition { changed: true }
        ) {
            return Err(background_result_error(
                "dead-letter background delivery",
                result,
            ));
        }
        self.prune_dead_letters(delivery, policy, now_unix_seconds)
    }

    fn prune_dead_letters(
        &self,
        delivery: &PluginBackgroundDelivery,
        policy: &BackgroundDeliveryPolicy,
        now_unix_seconds: i64,
    ) -> Result<()> {
        let retention = i64::try_from(policy.dead_letter_retention_seconds).unwrap_or(i64::MAX);
        let cutoff = unix_timestamp(now_unix_seconds.saturating_sub(retention));
        let result = self.app.relational.execute(
            RelationalOperation::PruneDeadLetters {
                plugin_id: delivery.plugin_id.clone(),
                export_id: delivery.export_id.clone(),
                updated_before: cutoff,
                max_entries: policy.dead_letter_max_entries,
            },
            &InvocationControl::sixty_seconds(),
        )?;
        if !matches!(result, RelationalResult::DeadLettersPruned { .. }) {
            return Err(background_result_error("prune dead letters", result));
        }
        Ok(())
    }
}

fn registration_spec(
    export: &PublishedBackgroundExport,

    now_unix_seconds: i64,
) -> PluginBackgroundRegistrationSpec {
    let (export_kind, contract, next_due_at) = match &export.kind {
        BackgroundExportKind::RecurringTask {
            interval_seconds,
            delivery,
        } => (
            "recurring_task",
            json!({"interval_seconds": interval_seconds, "delivery": delivery}),
            Some(now_unix_seconds),
        ),
        BackgroundExportKind::StorageTrigger {
            event_kinds,
            entity_kinds,
            delivery,
        } => (
            "change_hook",
            json!({"event_kinds": event_kinds, "entity_kinds": entity_kinds,
                "delivery": delivery}),
            None,
        ),
    };
    PluginBackgroundRegistrationSpec {
        plugin_id: export.plugin_id.clone(),
        export_id: export.export_id.clone(),
        export_kind: export_kind.into(),
        contract,
        next_due_at,
    }
}

fn background_result_error(operation: &str, result: RelationalResult) -> AppCoreError {
    AppCoreError::unsupported(
        operation,
        format!("unexpected relational persistence result: {result:?}"),
    )
}

pub(crate) fn sync_background_registrations(
    relational: &dyn RelationalPersistence,
    exports: &[PublishedBackgroundExport],
    now_unix_seconds: i64,
) -> Result<()> {
    if exports.len() > MAX_BACKGROUND_EXPORTS {
        return Err(AppCoreError::invalid_value(
            exports.len().to_string(),
            "at most 256 ready signed background exports",
        ));
    }
    let registrations = exports
        .iter()
        .map(|export| registration_spec(export, now_unix_seconds))
        .collect::<Vec<_>>();
    match relational.execute(
        RelationalOperation::SyncBackgroundRegistrations { registrations },
        &InvocationControl::sixty_seconds(),
    )? {
        RelationalResult::BackgroundRegistrations(_) => Ok(()),
        other => Err(AppCoreError::unsupported(
            "sync background registrations",
            format!("background registrations result, got {other:?}"),
        )),
    }
}

fn export_policies(
    exports: &[PublishedBackgroundExport],
) -> HashMap<(String, String), BackgroundDeliveryPolicy> {
    exports
        .iter()
        .map(|export| {
            (
                (export.plugin_id.clone(), export.export_id.clone()),
                export.kind.delivery().clone(),
            )
        })
        .collect()
}

fn recurring_delivery_id(export: &PublishedBackgroundExport, scheduled_at: i64) -> String {
    format!(
        "recurring:{}:{}:{scheduled_at}",
        export.plugin_id, export.export_id
    )
}

fn invocation_lease_seconds(timeout: Duration) -> i64 {
    i64::try_from(timeout.as_millis().saturating_add(999) / 1_000)
        .unwrap_or(i64::MAX)
        .saturating_add(5)
}

fn retry_delay_seconds(policy: &BackgroundDeliveryPolicy, attempts: u32) -> i64 {
    let exponent = attempts.saturating_sub(1).min(31);
    let multiplier = 1_u64 << exponent;
    let delay_ms = policy
        .initial_backoff_ms
        .saturating_mul(multiplier)
        .min(policy.max_backoff_ms);
    i64::try_from(delay_ms.saturating_add(999) / 1_000).unwrap_or(i64::MAX)
}

fn failed_run(delivery: PluginBackgroundDelivery, output: Value) -> BackgroundDeliveryRun {
    BackgroundDeliveryRun {
        delivery,
        status: PluginInvocationStatus::Failed,
        output,
    }
}

fn unix_timestamp(value: i64) -> String {
    chrono::DateTime::<Utc>::from_timestamp(value, 0)
        .unwrap_or(chrono::DateTime::<Utc>::MIN_UTC)
        .to_rfc3339()
}
