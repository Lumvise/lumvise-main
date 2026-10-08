//! Owns the canonical JSON input to Knowledge projection hashes.
//! Projection owners call `canonical_projection_json`; output payloads remain unchanged.

use serde_json::Value;

/// Keeps projection identities independent of Cargo's JSON map-order feature.
/// Example: `canonical_projection_json(&serde_json::json!({"z": 1, "a": 2}))`.
pub(crate) fn canonical_projection_json(value: &Value) -> String {
    let mut canonical = value.clone();
    canonical.sort_all_objects();
    canonical.to_string()
}

#[cfg(test)]
mod tests {
    use super::canonical_projection_json;
    use serde_json::json;

    #[test]
    fn nested_object_order_is_canonical_and_array_order_is_retained() {
        let original = json!({"z": [{"y": 1, "a": 2}, "second"], "a": true});
        let original_json = original.to_string();
        let reordered = json!({"a": true, "z": [{"a": 2, "y": 1}, "second"]});
        let expected = r#"{"a":true,"z":[{"a":2,"y":1},"second"]}"#;
        assert_eq!(canonical_projection_json(&original), expected);
        assert_eq!(canonical_projection_json(&reordered), expected);
        assert_eq!(canonical_projection_json(&json!([2, 1])), "[2,1]");
        assert_eq!(original.to_string(), original_json);
    }
}
