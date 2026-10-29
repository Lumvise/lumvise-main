//! Conditional row I/O for named test hosts; semantic revision rules stay in the real policy tests.
use lumvise_plugin_sdk::PluginError;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) fn apply(
    rows: &mut BTreeMap<(String, String), Value>,
    input: &Value,
) -> Result<Value, PluginError> {
    let mut staged = rows.clone();
    let (mut rows_put, mut rows_deleted) = (0, 0);
    for mutation in input["mutations"].as_array().expect("SDK mutations array") {
        let (put, deleted) = apply_mutation(&mut staged, mutation)?;
        rows_put += put;
        rows_deleted += deleted;
    }
    *rows = staged;
    Ok(json!({"rows_put":rows_put,"rows_deleted":rows_deleted}))
}

fn apply_mutation(
    rows: &mut BTreeMap<(String, String), Value>,
    mutation: &Value,
) -> Result<(u64, u64), PluginError> {
    let key = (
        mutation["table_name"].as_str().unwrap().into(),
        mutation["row_key"].as_str().unwrap().into(),
    );
    match mutation["operation"].as_str().unwrap() {
        "require_value" => {
            require_value(rows.get(&key), &mutation["expected"])?;
            Ok((0, 0))
        }
        "put" => {
            rows.insert(key, mutation["value"].clone());
            Ok((1, 0))
        }
        "delete" => Ok((0, u64::from(rows.remove(&key).is_some()))),
        other => panic!("unexpected fake row mutation `{other}`"),
    }
}

fn require_value(actual: Option<&Value>, expected: &Value) -> Result<(), PluginError> {
    let matches = match expected["presence"].as_str().unwrap() {
        "absent" => actual.is_none(),
        "present" => actual == Some(&expected["value"]),
        other => panic!("unexpected fake row presence `{other}`"),
    };
    if matches {
        return Ok(());
    }
    Err(PluginError::new(
        "relational_condition_failed",
        format!("row value `{actual:?}`; expected `{expected}`"),
        false,
    ))
}
