use super::*;
use serde::de::DeserializeOwned;

fn unchanged<T: DeserializeOwned + Serialize>(json: &str) {
    let value: T = serde_json::from_str(json).unwrap();
    assert_eq!(serde_json::to_string(&value).unwrap(), json);
}

#[test]
fn blob_retains_byte_values_and_existing_field_order() {
    unchanged::<ArtifactBlob>(
        r#"{"content_ref":"blob","artifact_id":"a","media_type":"application/octet-stream","content":[0,128,255],"updated_at":"2026-10-26T00:00:00Z"}"#,
    );
}

#[test]
fn vector_payloads_retain_existing_serialized_shapes_and_large_metadata_numbers() {
    let vector = r#"{"engine_id":"engine","model":null,"dimensions":2,"vector":[0.5,-1.0],"normalized":false,"metadata":{"counter":18446744073709551615}}"#;
    unchanged::<ArtifactTextVector>(vector);
    unchanged::<StoredArtifactTextVector>(&format!(
        r#"{{"artifact_id":"a","semantic_element_id":"e","source_text":"text","vector":{vector}}}"#,
    ));
    unchanged::<StoredSemanticElementNameVector>(&format!(
        r#"{{"semantic_element_id":"e","project_root":"/source","source_text":"name","vector":{vector}}}"#,
    ));
}
