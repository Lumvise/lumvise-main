//! Durable identity and conditional scope rows; only the authority owner calls this module.
use lumvise_db_core::{
    PluginDataExpectedValue, PluginDataMutation, RelationalOperation, RelationalPersistence,
    RelationalResult,
};
use lumvise_plugin_runtime::HostCapabilityError;
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};

const OWNER: &str = "app.knowledge-authority";
const TABLE: &str = "memory_authority";

/// Checks opaque owner facts without interpreting another plugin's row format.
/// Example: `require_owner_observation(store, owner, table, key, Some(&observed), control)`.
pub(super) fn require_owner_observation(
    store: &dyn RelationalPersistence,
    owner: &str,
    table: &str,
    key: &str,
    expected: Option<&Value>,
    control: &InvocationControl,
) -> Result<(), HostCapabilityError> {
    if owner_observation_matches(store, owner, table, key, expected, control)? {
        return Ok(());
    }
    Err(super::invalid(
        &json!({"owner":owner,"table":table,"key":key}),
        "the unchanged original owner observation",
    ))
}

/// Distinguishes an unaccepted future fact from a storage failure during setup recovery.
/// Example: absent acceptance returns false; an unavailable store remains an error.
pub(super) fn owner_observation_matches(
    store: &dyn RelationalPersistence,
    owner: &str,
    table: &str,
    key: &str,
    expected: Option<&Value>,
    control: &InvocationControl,
) -> Result<bool, HostCapabilityError> {
    let operation = RelationalOperation::GetPluginData {
        plugin_id: owner.into(),
        table_name: table.into(),
        row_key: key.into(),
    };
    match store.execute(operation, control).map_err(failure)? {
        RelationalResult::PluginDataRow(row) => Ok(row.as_ref().map(|row| &row.value) == expected),
        result => Err(super::invalid(&json!(result), "an owner row result")),
    }
}

pub(super) fn read(
    store: &dyn RelationalPersistence,
    key: &str,
    control: &InvocationControl,
) -> Result<Option<Value>, HostCapabilityError> {
    match store
        .execute(
            RelationalOperation::GetPluginData {
                plugin_id: OWNER.into(),
                table_name: TABLE.into(),
                row_key: key.into(),
            },
            control,
        )
        .map_err(failure)?
    {
        RelationalResult::PluginDataRow(row) => Ok(row.map(|row| row.value)),
        result => Err(super::invalid(&json!(result), "authority row result")),
    }
}

/// Reads only authority rows in a named namespace. Example: pending delegation recovery.
pub(super) fn list_prefix(
    store: &dyn RelationalPersistence,
    prefix: &str,
    control: &InvocationControl,
) -> Result<Vec<Value>, HostCapabilityError> {
    let mut values = Vec::new();
    let mut after_key = None;
    loop {
        let operation = RelationalOperation::PagePluginData {
            plugin_id: OWNER.into(),
            table_name: TABLE.into(),
            key_prefix: Some(prefix.into()),
            after_key,
            limit: 100,
        };
        let RelationalResult::PluginDataPage(page) =
            store.execute(operation, control).map_err(failure)?
        else {
            return Err(super::invalid(&json!(prefix), "authority page result"));
        };
        values.extend(page.rows.into_iter().map(|row| row.value));
        after_key = page.next_after_key;
        if after_key.is_none() {
            return Ok(values);
        }
    }
}

pub(super) fn ensure(
    store: &dyn RelationalPersistence,
    control: &InvocationControl,
) -> Result<(), HostCapabilityError> {
    store
        .execute(
            RelationalOperation::EnsurePluginDataTable {
                plugin_id: OWNER.into(),
                table_name: TABLE.into(),
                schema: json!({"type":"object"}),
            },
            control,
        )
        .map_err(failure)?;
    Ok(())
}

pub(super) fn replace(
    store: &dyn RelationalPersistence,
    key: &str,
    previous: Option<Value>,
    value: Value,
    control: &InvocationControl,
) -> Result<(), HostCapabilityError> {
    let expected = previous.map_or(
        PluginDataExpectedValue::Absent,
        PluginDataExpectedValue::Present,
    );
    store
        .execute(
            RelationalOperation::ApplyMutations {
                plugin_id: OWNER.into(),
                mutations: vec![
                    PluginDataMutation::RequireValue {
                        table_name: TABLE.into(),
                        row_key: key.into(),
                        expected,
                    },
                    PluginDataMutation::Put {
                        table_name: TABLE.into(),
                        row_key: key.into(),
                        value,
                    },
                ],
            },
            control,
        )
        .map_err(failure)?;
    Ok(())
}

/// Atomically records a permission change and its original retry receipt.
/// Example: neither a new delegation nor its receipt can survive alone.
pub(super) fn replace_pair(
    store: &dyn RelationalPersistence,
    first: (&str, Option<Value>, Value),
    second: (&str, Option<Value>, Value),
    control: &InvocationControl,
) -> Result<(), HostCapabilityError> {
    let mutations = [first, second]
        .into_iter()
        .flat_map(|(key, previous, value)| {
            [
                PluginDataMutation::RequireValue {
                    table_name: TABLE.into(),
                    row_key: key.into(),
                    expected: previous.map_or(
                        PluginDataExpectedValue::Absent,
                        PluginDataExpectedValue::Present,
                    ),
                },
                PluginDataMutation::Put {
                    table_name: TABLE.into(),
                    row_key: key.into(),
                    value,
                },
            ]
        })
        .collect();
    store
        .execute(
            RelationalOperation::ApplyMutations {
                plugin_id: OWNER.into(),
                mutations,
            },
            control,
        )
        .map_err(failure)?;
    Ok(())
}

fn failure(error: lumvise_db_core::DbError) -> HostCapabilityError {
    HostCapabilityError::new(
        super::CAPABILITY,
        "knowledge_authority_storage_failed",
        format!("memory authority persistence failed: {error}"),
        false,
    )
}
