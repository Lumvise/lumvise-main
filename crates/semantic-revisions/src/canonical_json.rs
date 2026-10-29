//! Map iteration differs with preserve_order; metadata order must not change revision identities.
use serde::{
    Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};
use serde_json::Value;

pub(crate) fn serialize_canonical_metadata<S: Serializer>(
    value: &Value,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    CanonicalMetadata(value).serialize(serializer)
}

pub(crate) struct CanonicalMetadata<'a>(pub(crate) &'a Value);

impl Serialize for CanonicalMetadata<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Object(entries) => serialize_ordered_object(entries, serializer),
            Value::Array(values) => {
                let mut array = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    array.serialize_element(&CanonicalMetadata(value))?;
                }
                array.end()
            }
            value => value.serialize(serializer),
        }
    }
}

fn serialize_ordered_object<S: Serializer>(
    entries: &serde_json::Map<String, Value>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut sorted = entries.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|(key, _)| *key);
    let mut object = serializer.serialize_map(Some(entries.len()))?;
    for (key, value) in sorted {
        object.serialize_entry(key, &CanonicalMetadata(value))?;
    }
    object.end()
}
