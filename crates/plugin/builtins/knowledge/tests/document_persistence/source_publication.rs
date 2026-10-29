//! Source fixtures use the real App Core checkpoint and Semantic publisher, without a policy fake.
use super::*;
use lumvise_contracts::{SourceIndexPreparation, SourceIndexPublication};
use sha2::{Digest, Sha256};

pub(super) fn publish(host: &DocumentBoundaryHost, batch: Value) -> Value {
    let Some(frozen) = freeze(host, batch) else {
        return json!({"accepted":true, "unchanged":true});
    };
    let provider = frozen.batch.provider_instance_id.clone();
    let mut page = serde_json::to_value(frozen.batch).unwrap();
    page["ingestion_job_id"] = json!(frozen.operation_id);
    page["ingestion_page_index"] = json!(0);
    page["ingestion_page_count"] = json!(1);
    let result = host.semantic("ingest_index_batch", page).unwrap();
    assert_eq!(result["accepted"], true, "{result}");
    acknowledge(host, &provider, &result["publication"]["value"]);
    result
}

pub(super) fn freeze(host: &DocumentBoundaryHost, batch: Value) -> Option<SourceIndexPublication> {
    let prepared: SourceIndexPreparation = serde_json::from_value(invoke(
        host,
        json!({"operation":"prepare",
        "folder_path":host.source_directory.path(), "project_root":batch["project_root"],
        "provider_instance_id":batch["provider_instance_id"]}),
    ))
    .unwrap();
    let SourceIndexPreparation::Scan {
        source, base_token, ..
    } = prepared
    else {
        panic!("expected no interrupted publication in this source fixture")
    };
    let projected = projected_batch(batch, &source.semantic_source_id, json!([source]));
    serde_json::from_value(invoke(
        host,
        json!({"operation":"freeze", "folder_path":host.source_directory.path(),
        "base_token":base_token, "full_snapshot":true, "batch":projected}),
    ))
    .unwrap()
}

pub(super) fn acknowledge(host: &DocumentBoundaryHost, provider: &str, receipt: &Value) {
    let result = invoke(
        host,
        json!({"operation":"acknowledge", "folder_path":host.source_directory.path(),
        "project_root":receipt["selection"]["project_root"], "provider_instance_id":provider, "receipt":receipt}),
    );
    assert_eq!(result["acknowledged"], true, "{result}");
}

fn invoke(host: &DocumentBoundaryHost, input: Value) -> Value {
    host.broker
        .invoke(HostCapabilityRequest {
            plugin_id: "builtin.semantic".into(),
            invocation_id: "source-fixture".into(),
            call_id: "source-fixture".into(),
            capability_id: "project.index".into(),
            required_version: "^1.0".into(),
            input,
        })
        .expect("real source publication capability")
}

fn projected_batch(mut batch: Value, source_id: &str, sources: Value) -> Value {
    batch["semantic_sources"] = sources;
    for element in batch["semantic_elements"].as_array_mut().unwrap() {
        element["semantic_source_id"] = json!(source_id);
        if element["semantic_element_type"] == "folder" {
            element["metadata"]["kind"] = json!("directory");
        }
        if element["semantic_element_type"] == "file" {
            let bytes = serde_json::to_vec(&(
                element["path"].clone(),
                element["content_fingerprint"].clone(),
            ))
            .unwrap();
            element["metadata"]["source_digest_sha256"] =
                json!(format!("{:x}", Sha256::digest(bytes)));
        }
    }
    batch
}
