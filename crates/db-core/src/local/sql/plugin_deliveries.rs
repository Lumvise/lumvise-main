//! Durable host-owned lifecycle for compiled-plugin background deliveries.

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;

use crate::local::sql::connections::SqlConnections;
use crate::local::sql::validation::require_non_empty;
use crate::{
    DbError, PluginBackgroundDelivery, PluginBackgroundRegistration,
    PluginBackgroundRegistrationSpec, Result,
};

const MAX_DUE_DELIVERIES: usize = 1_000;
const MAX_BACKGROUND_REGISTRATIONS: usize = 256;

/// SQL repository for idempotent background registration, leasing, and outcomes.
pub struct PluginDeliveryRepository<'db> {
    conn: &'db SqlConnections,
}

impl<'db> PluginDeliveryRepository<'db> {
    pub(crate) fn new(conn: &'db SqlConnections) -> Self {
        Self { conn }
    }

    /// Atomically activates the supplied ready catalog and deactivates stale entries.
    pub fn sync_registrations(
        &self,
        registrations: &[PluginBackgroundRegistrationSpec],
    ) -> Result<()> {
        if registrations.len() > MAX_BACKGROUND_REGISTRATIONS {
            return Err(DbError::invalid_value(
                registrations.len().to_string(),
                "at most 256 active background registrations",
            ));
        }
        for registration in registrations {
            validate_registration(registration)?;
        }
        if registrations_match_active(&self.conn.read_conn(), registrations)? {
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.write_conn();
        let transaction = conn.transaction()?;
        transaction.execute("UPDATE plugin_background_registrations SET active = 0", [])?;
        for registration in registrations {
            upsert_registration(&transaction, registration, &now)?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Lists active registrations in deterministic identity order.
    pub fn active_registrations(&self) -> Result<Vec<PluginBackgroundRegistration>> {
        let conn = self.conn.read_conn();
        let mut statement = conn.prepare_cached(
            "SELECT plugin_id, export_id, export_kind, contract_json, active, next_due_at,
                    created_at, updated_at
             FROM plugin_background_registrations WHERE active = 1
             ORDER BY plugin_id, export_id",
        )?;
        let rows = statement.query_map([], registration_row)?;
        rows.map(|row| registration_from_parts(row?)).collect()
    }

    /// Enqueues one due recurring slot and advances its durable cadence cursor atomically.
    pub fn enqueue_recurring(
        &self,
        plugin_id: &str,
        export_id: &str,
        scheduled_at: i64,
        now_unix_seconds: i64,
        interval_seconds: u64,
        payload: &Value,
    ) -> Result<Option<PluginBackgroundDelivery>> {
        validate_delivery_ref(plugin_id, export_id)?;
        let interval = i64::try_from(interval_seconds)
            .map_err(|_| DbError::invalid_value(interval_seconds.to_string(), "i64 cadence"))?;
        if interval == 0 {
            return Err(DbError::invalid_value(
                interval_seconds.to_string(),
                "positive recurring cadence",
            ));
        }
        let delivery_id = format!("recurring:{plugin_id}:{export_id}:{scheduled_at}");
        let source_key = format!("scheduled:{scheduled_at}");
        let mut conn = self.conn.write_conn();
        let transaction = conn.transaction()?;
        insert_registered_delivery(
            &transaction,
            DeliveryInsert {
                delivery_id: &delivery_id,
                plugin_id,
                export_id,
                kind: "recurring_task",
                source_key: &source_key,
                payload,
                next_attempt_at: scheduled_at,
            },
        )?;
        transaction.execute(
            "UPDATE plugin_background_registrations
             SET next_due_at = ?3, updated_at = ?4
             WHERE plugin_id = ?1 AND export_id = ?2 AND active = 1
               AND export_kind = 'recurring_task' AND next_due_at = ?5",
            params![
                plugin_id,
                export_id,
                next_recurring_due_at(scheduled_at, now_unix_seconds, interval),
                Utc::now().to_rfc3339(),
                scheduled_at,
            ],
        )?;
        let delivery = delivery_by_id(&transaction, &delivery_id)?;
        transaction.commit()?;
        Ok(delivery)
    }

    /// Idempotently enqueues change-hook element deliveries for an active signed
    /// trigger. The source keys are opaque hook-generation identities.
    pub fn enqueue_change_hook_batch(
        &self,
        plugin_id: &str,
        export_id: &str,
        now_unix_seconds: i64,
        deliveries: &[(String, Value)],
    ) -> Result<usize> {
        validate_delivery_ref(plugin_id, export_id)?;
        let mut conn = self.conn.write_conn();
        let transaction = conn.transaction()?;
        let mut inserted = 0;
        for (source_key, payload) in deliveries {
            require_non_empty(source_key, "non-empty change-hook delivery source key")?;
            inserted += insert_registered_delivery(
                &transaction,
                DeliveryInsert {
                    delivery_id: &change_hook_delivery_id(plugin_id, export_id, source_key),
                    plugin_id,
                    export_id,
                    kind: "change_hook",
                    source_key,
                    payload,
                    next_attempt_at: now_unix_seconds,
                },
            )?;
        }
        transaction.commit()?;
        Ok(inserted)
    }

    /// Lists bounded due or expired-lease deliveries for active registrations.
    #[cfg(test)]
    pub fn due_deliveries(
        &self,
        now_unix_seconds: i64,
        limit: usize,
    ) -> Result<Vec<PluginBackgroundDelivery>> {
        self.due_deliveries_for_kind(now_unix_seconds, limit, None)
    }

    /// Lists bounded due deliveries for exactly one background queue.
    pub fn due_deliveries_by_kind(
        &self,
        delivery_kind: &str,
        now_unix_seconds: i64,
        limit: usize,
    ) -> Result<Vec<PluginBackgroundDelivery>> {
        require_non_empty(delivery_kind, "non-empty background delivery kind")?;
        self.due_deliveries_for_kind(now_unix_seconds, limit, Some(delivery_kind))
    }

    fn due_deliveries_for_kind(
        &self,
        now_unix_seconds: i64,
        limit: usize,
        delivery_kind: Option<&str>,
    ) -> Result<Vec<PluginBackgroundDelivery>> {
        if !(1..=MAX_DUE_DELIVERIES).contains(&limit) {
            return Err(DbError::invalid_value(
                limit.to_string(),
                "due delivery limit from 1 through 1000",
            ));
        }
        let conn = self.conn.read_conn();
        let mut statement = conn.prepare_cached(
            "SELECT d.delivery_id, d.plugin_id, d.export_id, d.delivery_kind, d.source_key,
                    d.payload_json, d.state, d.attempts, d.next_attempt_at,
                    d.lease_expires_at, d.last_error, d.created_at, d.updated_at
             FROM plugin_background_deliveries d
             JOIN plugin_background_registrations r
               ON r.plugin_id = d.plugin_id AND r.export_id = d.export_id
             WHERE r.active = 1 AND (?3 IS NULL OR d.delivery_kind = ?3) AND (
                 (d.state IN ('pending', 'retry_wait') AND d.next_attempt_at <= ?1)
                 OR (d.state = 'in_flight' AND d.lease_expires_at <= ?1)
             ) ORDER BY d.next_attempt_at, d.delivery_id LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![
                now_unix_seconds,
                i64::try_from(limit).unwrap_or(i64::MAX),
                delivery_kind
            ],
            delivery_row,
        )?;
        rows.map(|row| delivery_from_parts(row?)).collect()
    }

    /// Claims one due delivery with a crash-recoverable lease and increments attempts.
    pub fn claim(
        &self,
        delivery_id: &str,
        now_unix_seconds: i64,
        lease_seconds: i64,
    ) -> Result<Option<PluginBackgroundDelivery>> {
        require_non_empty(delivery_id, "non-empty delivery id")?;
        if lease_seconds <= 0 {
            return Err(DbError::invalid_value(
                lease_seconds.to_string(),
                "positive delivery lease seconds",
            ));
        }
        let mut conn = self.conn.write_conn();
        let transaction = conn.transaction()?;
        let changed = transaction.execute(
            "UPDATE plugin_background_deliveries SET state = 'in_flight',
             attempts = attempts + 1, lease_expires_at = ?2, updated_at = ?3
             WHERE delivery_id = ?1 AND (
                 (state IN ('pending', 'retry_wait') AND next_attempt_at <= ?4)
                 OR (state = 'in_flight' AND lease_expires_at <= ?4)
             ) AND EXISTS (
                 SELECT 1 FROM plugin_background_registrations r
                 WHERE r.plugin_id = plugin_background_deliveries.plugin_id
                   AND r.export_id = plugin_background_deliveries.export_id AND r.active = 1
             )",
            params![
                delivery_id,
                now_unix_seconds.saturating_add(lease_seconds),
                Utc::now().to_rfc3339(),
                now_unix_seconds,
            ],
        )?;
        let delivery = (changed == 1)
            .then(|| delivery_by_id(&transaction, delivery_id))
            .transpose()?
            .flatten();
        transaction.commit()?;
        Ok(delivery)
    }

    /// Marks one claimed delivery completed exactly once.
    pub fn complete(&self, delivery_id: &str) -> Result<bool> {
        self.complete_at(delivery_id, Utc::now().timestamp())
    }

    /// Marks one claimed delivery completed using the scheduler clock.
    pub fn complete_at(&self, delivery_id: &str, now_unix_seconds: i64) -> Result<bool> {
        self.finish_delivery_at(delivery_id, "completed", None, None, now_unix_seconds)
    }

    /// Schedules another attempt for one claimed delivery.
    pub fn retry(&self, delivery_id: &str, next_attempt_at: i64, error: &str) -> Result<bool> {
        self.retry_at(delivery_id, next_attempt_at, error, Utc::now().timestamp())
    }

    /// Schedules another attempt using the scheduler clock.
    pub fn retry_at(
        &self,
        delivery_id: &str,
        next_attempt_at: i64,
        error: &str,
        now_unix_seconds: i64,
    ) -> Result<bool> {
        self.finish_delivery_at(
            delivery_id,
            "retry_wait",
            Some(next_attempt_at),
            Some(error),
            now_unix_seconds,
        )
    }

    /// Moves one claimed delivery into durable dead-letter state.
    pub fn dead_letter(&self, delivery_id: &str, error: &str) -> Result<bool> {
        self.dead_letter_at(delivery_id, error, Utc::now().timestamp())
    }

    /// Moves one claimed delivery to dead letter using the scheduler clock.
    pub fn dead_letter_at(
        &self,
        delivery_id: &str,
        error: &str,
        now_unix_seconds: i64,
    ) -> Result<bool> {
        self.finish_delivery_at(
            delivery_id,
            "dead_letter",
            None,
            Some(error),
            now_unix_seconds,
        )
    }

    /// Prunes expired and over-quota dead letters for one export.
    pub fn prune_dead_letters(
        &self,
        plugin_id: &str,
        export_id: &str,
        updated_before: &str,
        max_entries: u32,
    ) -> Result<usize> {
        validate_delivery_ref(plugin_id, export_id)?;
        let conn = self.conn.write_conn();
        let expired = conn.execute(
            "DELETE FROM plugin_background_deliveries
             WHERE plugin_id = ?1 AND export_id = ?2 AND state = 'dead_letter'
               AND updated_at < ?3",
            params![plugin_id, export_id, updated_before],
        )?;
        let overflow = conn.execute(
            "DELETE FROM plugin_background_deliveries WHERE delivery_id IN (
                 SELECT delivery_id FROM plugin_background_deliveries
                 WHERE plugin_id = ?1 AND export_id = ?2 AND state = 'dead_letter'
                 ORDER BY updated_at DESC, delivery_id DESC LIMIT -1 OFFSET ?3
             )",
            params![plugin_id, export_id, i64::from(max_entries)],
        )?;
        Ok(expired + overflow)
    }

    /// Reads one delivery for diagnostics and tests.
    pub fn delivery(&self, delivery_id: &str) -> Result<Option<PluginBackgroundDelivery>> {
        require_non_empty(delivery_id, "non-empty delivery id")?;
        delivery_by_id(&self.conn.read_conn(), delivery_id)
    }

    fn finish_delivery_at(
        &self,
        delivery_id: &str,
        state: &str,
        next_attempt_at: Option<i64>,
        error: Option<&str>,
        now_unix_seconds: i64,
    ) -> Result<bool> {
        require_non_empty(delivery_id, "non-empty delivery id")?;
        let conn = self.conn.write_conn();
        let changed = conn.execute(
            "UPDATE plugin_background_deliveries SET state = ?2,
             next_attempt_at = COALESCE(?3, next_attempt_at), lease_expires_at = NULL,
             last_error = ?4, updated_at = ?5
             WHERE delivery_id = ?1 AND state = 'in_flight'",
            params![
                delivery_id,
                state,
                next_attempt_at,
                error,
                unix_timestamp(now_unix_seconds),
            ],
        )?;
        Ok(changed == 1)
    }
}

fn registrations_match_active(
    read_conn: &rusqlite::Connection,
    registrations: &[PluginBackgroundRegistrationSpec],
) -> Result<bool> {
    let mut statement = read_conn.prepare_cached(
        "SELECT plugin_id, export_id, export_kind, contract_json
         FROM plugin_background_registrations
         WHERE active = 1 ORDER BY plugin_id, export_id",
    )?;
    let rows = statement.query_map([], existing_registration_row)?;
    let mut existing: Vec<(String, String, String, String)> = Vec::new();
    for row in rows {
        existing.push(row?);
    }
    if existing.len() != registrations.len() {
        return Ok(false);
    }
    let mut incoming = registrations
        .iter()
        .map(|registration| {
            let contract = registration.contract.to_string();
            (
                registration.plugin_id.clone(),
                registration.export_id.clone(),
                registration.export_kind.clone(),
                contract,
            )
        })
        .collect::<Vec<_>>();
    incoming.sort_unstable_by(|left, right| {
        (left.0.as_str(), left.1.as_str()).cmp(&(right.0.as_str(), right.1.as_str()))
    });
    Ok(incoming
        .iter()
        .zip(existing.iter())
        .all(|(incoming, existing)| {
            incoming.0 == existing.0
                && incoming.1 == existing.1
                && incoming.2 == existing.2
                && incoming.3 == existing.3
        }))
}

fn existing_registration_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(String, String, String, String)> {
    let plugin_id = row.get(0)?;
    let export_id = row.get(1)?;
    let export_kind = row.get(2)?;
    let contract_json = row.get::<_, String>(3)?;
    Ok((plugin_id, export_id, export_kind, contract_json))
}

fn next_recurring_due_at(scheduled_at: i64, now_unix_seconds: i64, interval: i64) -> i64 {
    let elapsed = now_unix_seconds.saturating_sub(scheduled_at).max(0);
    let skipped_intervals = elapsed.saturating_div(interval).saturating_add(1);
    scheduled_at.saturating_add(interval.saturating_mul(skipped_intervals))
}

fn validate_registration(registration: &PluginBackgroundRegistrationSpec) -> Result<()> {
    validate_delivery_ref(&registration.plugin_id, &registration.export_id)?;
    if !matches!(
        registration.export_kind.as_str(),
        "recurring_task" | "change_hook"
    ) {
        return Err(DbError::invalid_value(
            &registration.export_kind,
            "recurring_task or change_hook",
        ));
    }
    if !registration.contract.is_object() {
        return Err(DbError::invalid_value(
            registration.contract.to_string(),
            "background registration contract object",
        ));
    }
    Ok(())
}

fn validate_delivery_ref(plugin_id: &str, export_id: &str) -> Result<()> {
    require_non_empty(plugin_id, "non-empty plugin id")?;
    require_non_empty(export_id, "non-empty export id")
}

fn upsert_registration(
    transaction: &Transaction<'_>,
    registration: &PluginBackgroundRegistrationSpec,
    now: &str,
) -> Result<()> {
    let contract = serde_json::to_string(&registration.contract)?;
    transaction.execute(
        "INSERT INTO plugin_background_registrations(
             plugin_id, export_id, export_kind, contract_json, active,
             next_due_at, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?6)
         ON CONFLICT(plugin_id, export_id) DO UPDATE SET
             export_kind = excluded.export_kind,
             next_due_at = CASE
                 WHEN plugin_background_registrations.contract_json <> excluded.contract_json
                 THEN excluded.next_due_at ELSE plugin_background_registrations.next_due_at END,
             contract_json = excluded.contract_json, active = 1, updated_at = excluded.updated_at",
        params![
            registration.plugin_id,
            registration.export_id,
            registration.export_kind,
            contract,
            registration.next_due_at,
            now,
        ],
    )?;
    Ok(())
}

fn change_hook_delivery_id(plugin_id: &str, export_id: &str, source_key: &str) -> String {
    format!("change_hook:{plugin_id}:{export_id}:{source_key}")
}

struct DeliveryInsert<'value> {
    delivery_id: &'value str,
    plugin_id: &'value str,
    export_id: &'value str,
    kind: &'value str,
    source_key: &'value str,
    payload: &'value Value,
    next_attempt_at: i64,
}

fn insert_registered_delivery(conn: &Connection, delivery: DeliveryInsert<'_>) -> Result<usize> {
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO plugin_background_deliveries(
             delivery_id, plugin_id, export_id, delivery_kind, source_key, payload_json,
             state, attempts, next_attempt_at, created_at, updated_at
         ) SELECT ?1, ?2, ?3, ?4, ?5, ?6, 'pending', 0, ?7, ?8, ?8
         WHERE EXISTS (
             SELECT 1 FROM plugin_background_registrations
             WHERE plugin_id = ?2 AND export_id = ?3 AND export_kind = ?4 AND active = 1
               AND (export_kind != 'recurring_task' OR next_due_at = ?7)
         )",
        params![
            delivery.delivery_id,
            delivery.plugin_id,
            delivery.export_id,
            delivery.kind,
            delivery.source_key,
            serde_json::to_string(delivery.payload)?,
            delivery.next_attempt_at,
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(inserted)
}

type RegistrationParts = (
    String,
    String,
    String,
    String,
    i64,
    Option<i64>,
    String,
    String,
);

fn registration_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RegistrationParts> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
    ))
}

fn registration_from_parts(parts: RegistrationParts) -> Result<PluginBackgroundRegistration> {
    Ok(PluginBackgroundRegistration {
        plugin_id: parts.0,
        export_id: parts.1,
        export_kind: parts.2,
        contract: serde_json::from_str(&parts.3)?,
        active: parts.4 != 0,
        next_due_at: parts.5,
        created_at: parts.6,
        updated_at: parts.7,
    })
}

fn unix_timestamp(value: i64) -> String {
    chrono::DateTime::<Utc>::from_timestamp(value, 0)
        .unwrap_or(chrono::DateTime::<Utc>::MIN_UTC)
        .to_rfc3339()
}

type DeliveryParts = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    Option<i64>,
    Option<String>,
    String,
    String,
);

fn delivery_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeliveryParts> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
    ))
}

fn delivery_from_parts(parts: DeliveryParts) -> Result<PluginBackgroundDelivery> {
    Ok(PluginBackgroundDelivery {
        delivery_id: parts.0,
        plugin_id: parts.1,
        export_id: parts.2,
        delivery_kind: parts.3,
        source_key: parts.4,
        payload: serde_json::from_str(&parts.5)?,
        state: parts.6,
        attempts: u32::try_from(parts.7)
            .map_err(|_| DbError::invalid_value(parts.7.to_string(), "u32 attempt count"))?,
        next_attempt_at: parts.8,
        lease_expires_at: parts.9,
        last_error: parts.10,
        created_at: parts.11,
        updated_at: parts.12,
    })
}

fn delivery_by_id(
    conn: &Connection,
    delivery_id: &str,
) -> Result<Option<PluginBackgroundDelivery>> {
    let mut statement = conn.prepare_cached(
        "SELECT delivery_id, plugin_id, export_id, delivery_kind, source_key,
                payload_json, state, attempts, next_attempt_at, lease_expires_at,
                last_error, created_at, updated_at
         FROM plugin_background_deliveries WHERE delivery_id = ?1",
    )?;
    let parts = statement
        .query_row(params![delivery_id], delivery_row)
        .optional()?;
    parts.map(delivery_from_parts).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::sql::connections::SqlConnections;
    use serde_json::json;

    fn registration(
        plugin_id: &str,
        export_id: &str,
        contract_value: &str,
    ) -> PluginBackgroundRegistrationSpec {
        PluginBackgroundRegistrationSpec {
            plugin_id: plugin_id.to_owned(),
            export_id: export_id.to_owned(),
            export_kind: "change_hook".to_owned(),
            contract: json!({"version": contract_value}),
            next_due_at: None,
        }
    }

    #[test]
    fn sync_registrations_skips_when_no_active_snapshot_changed() {
        let connections = SqlConnections::in_memory().expect("database");
        let repository = PluginDeliveryRepository::new(&connections);
        let entries = vec![registration("plugin", "export", "first")];
        repository
            .sync_registrations(&entries)
            .expect("sync active registrations");

        let first_updated_at = repository
            .active_registrations()
            .expect("active registrations")
            .pop()
            .expect("one registration")
            .updated_at;

        repository
            .sync_registrations(&entries)
            .expect("sync unchanged registrations");
        let second_updated_at = repository
            .active_registrations()
            .expect("active registrations")
            .pop()
            .expect("one registration")
            .updated_at;

        assert_eq!(first_updated_at, second_updated_at);
    }

    #[test]
    fn sync_registrations_updates_when_contract_changes() {
        let connections = SqlConnections::in_memory().expect("database");
        let repository = PluginDeliveryRepository::new(&connections);
        let entries = vec![registration("plugin", "export", "first")];
        repository
            .sync_registrations(&entries)
            .expect("sync active registrations");
        let changed = vec![registration("plugin", "export", "changed")];
        repository
            .sync_registrations(&changed)
            .expect("sync changed registrations");

        assert_eq!(
            repository
                .active_registrations()
                .expect("active registrations")
                .pop()
                .expect("one registration")
                .contract,
            json!({"version":"changed"})
        );
    }
}
