use chrono::Utc;
use lumvise_plugin_sdk::{
    PluginContext, PluginError, StorageTriggerDisposition, StorageTriggerRequest,
};
use serde_json::{Value, json};
const RETAINED_LIVE_EVENTS: usize = 10_000;

use crate::{cultivation, md_nucleus, storage};

pub(crate) fn storage_change(
    trigger_id: &str,
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request = StorageTriggerRequest::from_value(input).map_err(|error| {
        PluginError::new(
            "invalid_storage_change_batch",
            format!("invalid StorageTrigger batch: {error}"),
            false,
        )
    })?;
    if request.project_root.trim().is_empty() {
        return Err(PluginError::new(
            "invalid_storage_change_batch",
            "StorageTrigger project_root must be non-empty",
            false,
        ));
    }
    let mut unique = std::collections::BTreeMap::new();
    for change in &request.changed {
        unique
            .entry(change.entity_id.clone())
            .or_insert_with(|| change.clone());
    }
    // Fingerprint matches are transfer suggestions. Indexing must never copy
    // artifacts behind the import review or resurrect a user's skipped choices.
    cultivation::refresh_reports_for_change_batch(&request, context)?;
    md_nucleus::reconcile_for_change_batch(&request, context)?;

    let mut rows = Vec::with_capacity(unique.len());
    for changed in unique.values() {
        let disposition = match changed.disposition {
            StorageTriggerDisposition::Upserted => "upserted",
            StorageTriggerDisposition::Removal => "removal",
        };
        let event = json!({
            "type": "knowledge.changed",
            "changeId": format!("{}:{}", request.target_revision, changed.entity_id),
            "eventKind": format!("{}.{}", changed.entity_kind, disposition),
            "entityKind": changed.entity_kind,
            "entityId": changed.entity_id,
            "sourceId": request.project_root,
            "triggerId": trigger_id,
            "occurredAt": Utc::now().to_rfc3339()
        });
        rows.push((
            format!("{}:{}", request.target_revision, event["entityId"]),
            event,
        ));
    }
    storage::put_rows(context, storage::LIVE_EVENTS, &rows)?;
    storage::trim_rows_by_key(context, storage::LIVE_EVENTS, RETAINED_LIVE_EVENTS)?;
    Ok(json!({"acknowledged": true, "events": rows.len()}))
}

pub(crate) fn live_events(
    cursor: Option<&str>,
    max_events: usize,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let selected = storage::page::<Value>(context, storage::LIVE_EVENTS, cursor, max_events)?;
    let next_cursor = selected
        .last()
        .and_then(|event| event["changeId"].as_str())
        .map(str::to_owned)
        .or_else(|| cursor.map(str::to_owned));
    let events = selected
        .into_iter()
        .map(|change| {
            json!({"id": change["changeId"], "event": "knowledge.changed",
            "data": change, "retry_ms": 1_000})
        })
        .collect::<Vec<_>>();
    Ok(json!({"events": events, "next_cursor": next_cursor, "done": false}))
}
