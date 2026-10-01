//! Binary representation for scoped records with arbitrary metadata values.
//! Bincode cannot deserialize serde_json::Value's untagged representation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{DbError, Result, SemanticScopedGraph};

#[derive(Serialize, Deserialize)]
pub(super) enum ScopedGraphValue {
    Null,
    Bool(bool),
    Signed(i64),
    Unsigned(u64),
    Float(f64),
    String(String),
    Array(Vec<Self>),
    Object(BTreeMap<String, Self>),
}

impl From<Value> for ScopedGraphValue {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(value) => Self::Bool(value),
            Value::Number(value) => {
                if let Some(value) = value.as_i64() {
                    return Self::Signed(value);
                }
                if let Some(value) = value.as_u64() {
                    return Self::Unsigned(value);
                }
                Self::Float(
                    value
                        .as_f64()
                        .expect("JSON number has a numeric representation"),
                )
            }
            Value::String(value) => Self::String(value),
            Value::Array(values) => Self::Array(values.into_iter().map(Self::from).collect()),
            Value::Object(values) => Self::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, Self::from(value)))
                    .collect(),
            ),
        }
    }
}

impl From<ScopedGraphValue> for Value {
    fn from(value: ScopedGraphValue) -> Self {
        match value {
            ScopedGraphValue::Null => Self::Null,
            ScopedGraphValue::Bool(value) => value.into(),
            ScopedGraphValue::Signed(value) => value.into(),
            ScopedGraphValue::Unsigned(value) => value.into(),
            ScopedGraphValue::Float(value) => value.into(),
            ScopedGraphValue::String(value) => value.into(),
            ScopedGraphValue::Array(values) => {
                Self::Array(values.into_iter().map(Self::from).collect())
            }
            ScopedGraphValue::Object(values) => Self::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, Self::from(value)))
                    .collect(),
            ),
        }
    }
}

pub(super) fn encode(graph: SemanticScopedGraph) -> Result<ScopedGraphValue> {
    Ok(serde_json::to_value(graph)?.into())
}

pub(super) fn decode(value: ScopedGraphValue) -> Result<SemanticScopedGraph> {
    serde_json::from_value(Value::from(value)).map_err(|error| {
        DbError::invalid_value(
            error.to_string(),
            "binary scoped semantic graph with portable record fields",
        )
    })
}
