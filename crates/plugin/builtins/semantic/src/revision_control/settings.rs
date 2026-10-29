//! A client preference for explicit cleanup, keyed by durable project identity.
//! Saving this preference never schedules collection or changes the source's selections.
use crate::storage;
use lumvise_plugin_sdk::{PluginContext, PluginError};
use lumvise_semantic_revisions::{SemanticRevisionRetentionPolicy, SemanticRevisionState};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const TABLE: &str = "semantic_retention_preferences";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreferenceRead {
    project_root: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreferenceWrite {
    project_root: String,
    expected_revision: u64,
    policy: SemanticRevisionRetentionPolicy,
}

#[derive(Deserialize, Serialize)]
struct RetentionPreference {
    project_id: String,
    revision: u64,
    policy: SemanticRevisionRetentionPolicy,
}

pub(super) fn read(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: PreferenceRead = crate::parse(input, "project_root")?;
    let project_id = project_identity(context, &request.project_root)?;
    let stored: Option<RetentionPreference> = storage::get(context, TABLE, &project_id)?;
    output(stored.unwrap_or(RetentionPreference {
        project_id,
        revision: 0,
        policy: SemanticRevisionRetentionPolicy::default(),
    }))
}

pub(super) fn write(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: PreferenceWrite =
        crate::parse(input, "project_root, original expected_revision and policy")?;
    let project_id = project_identity(context, &request.project_root)?;
    let original: Option<Value> = storage::get(context, TABLE, &project_id)?;
    let preference = revised_preference(&project_id, &request, original.as_ref())?;
    storage::apply_mutations(
        context,
        &[
            storage::require_row_mutation(TABLE, &project_id, original.as_ref()),
            storage::put_mutation(TABLE, &project_id, &preference)?,
        ],
    )?;
    output(preference)
}

fn revised_preference(
    project_id: &str,
    request: &PreferenceWrite,
    original: Option<&Value>,
) -> Result<RetentionPreference, PluginError> {
    let stored = original
        .map(|value| {
            crate::parse::<RetentionPreference>(value.clone(), "a stored retention preference")
        })
        .transpose()?;
    let revision = stored.as_ref().map_or(0, |preference| preference.revision);
    if revision != request.expected_revision
        || stored
            .as_ref()
            .is_some_and(|row| row.project_id != project_id)
    {
        return Err(PluginError::new(
            "retention_preference_conflict",
            format!(
                "retention preference {} is stale; expected current revision {revision} for {project_id}",
                request.expected_revision
            ),
            false,
        ));
    }
    let state = SemanticRevisionState {
        project_root: request.project_root.clone(),
        project_id: project_id.into(),
        ..Default::default()
    };
    state
        .plan_retention(
            "validate-preference",
            &request.policy,
            chrono::Utc::now().timestamp_millis(),
        )
        .map_err(|error| PluginError::new("invalid_retention_policy", error.to_string(), false))?;
    let revision = revision.checked_add(1).ok_or_else(|| {
        PluginError::new(
            "invalid_retention_preference",
            format!("revision {revision}: expected an incrementable preference revision"),
            false,
        )
    })?;
    Ok(RetentionPreference {
        project_id: project_id.into(),
        revision,
        policy: request.policy.clone(),
    })
}

fn project_identity(context: &mut PluginContext<'_>, root: &str) -> Result<String, PluginError> {
    crate::require_non_empty(root, "project_root")?;
    let response = context.host_call(
        "storage.semantic",
        json!({"operation":"project_identity", "project_root":root}),
    )?;
    response["identity"]["project_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| {
            PluginError::new(
                "invalid_semantic_storage",
                format!("project identity {response}: expected identity.project_id"),
                false,
            )
        })
}

fn output(preference: RetentionPreference) -> Result<Value, PluginError> {
    Ok(json!({"preference":preference, "automatic_collection":false}))
}
