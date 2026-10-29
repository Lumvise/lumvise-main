use super::*;
use lumvise_plugin_sdk::HostCallTransport;

struct ProjectionRaceHost {
    saved: Option<Value>,
    staged: Option<Value>,
    another_instance_committed: bool,
}

impl HostCallTransport for ProjectionRaceHost {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        if capability == "storage.semantic" {
            return Ok(json!({"identity":{"project_id":"lineage-original"}}));
        }
        match input["operation"].as_str().unwrap() {
            "ensure_table" => Ok(json!({})),
            "get_row" => Ok(json!({"row":self.saved.as_ref().map(|value| json!({"value":value}))})),
            "begin_mutations" => Ok(json!({"transaction_id":"projection"})),
            "stage_mutations" => {
                self.staged = Some(input["mutations"][1]["value"].clone());
                Ok(json!({}))
            }
            "commit_mutations" => self.competing_commit(),
            other => panic!("unexpected projection request {other}"),
        }
    }
}

impl ProjectionRaceHost {
    fn competing_commit(&mut self) -> Result<Value, PluginError> {
        if self.another_instance_committed {
            self.saved = self.staged.take();
        }
        Err(PluginError::new(
            "conditional_conflict",
            "another instance won the projection write",
            false,
        ))
    }
}

#[test]
fn concurrent_source_projection_adopts_only_an_already_projected_receipt() {
    let mut commit = accepted();
    commit["sources"] = json!([{"semantic_source_id":"source", "kind":"virtual_documents", "name":"Documents", "root_path":null, "root_uri":"lumvise:documents/lineage-original"}]);
    for another_instance_committed in [false, true] {
        let mut host = ProjectionRaceHost {
            saved: None,
            staged: None,
            another_instance_committed,
        };
        let result = publish(
            &mut PluginContext::for_test(&mut host),
            "/work/project",
            &commit,
        );
        if another_instance_committed {
            assert_eq!(result.unwrap(), 1);
        } else {
            assert_eq!(result.unwrap_err().code, "source_projection_retry");
        }
    }
}

fn accepted() -> Value {
    json!({"project_root":"/work/project", "project_id":"lineage-original",
        "publication":{"outcome":"accepted", "value":{
            "operation_id":"source-op", "commit_version":7}}})
}

#[test]
fn source_projection_rejects_missing_or_malformed_accepted_receipt_fields() {
    for (pointer, value) in [
        ("/project_root", json!("/another/project")),
        ("/project_id", Value::Null),
        ("/project_id", json!("")),
        ("/publication/outcome", json!("conflict")),
        ("/publication/value/operation_id", json!("")),
        ("/publication/value/commit_version", json!(-1)),
        ("/publication/value/commit_version", json!("7")),
    ] {
        assert_invalid_projection(pointer, value);
    }
}

fn assert_invalid_projection(pointer: &str, value: Value) {
    let mut commit = accepted();
    *commit.pointer_mut(pointer).unwrap() = value;
    let error = SourceDescriptorProjection::capture("/work/project", &commit)
        .err()
        .expect("malformed receipt must not produce descriptor writes");
    assert!(error.message.contains("expected"), "{}", error.message);
    assert!(!error.retryable);
}

#[test]
fn source_projection_preserves_original_lineage_and_receipt_order() {
    let projection = SourceDescriptorProjection::capture("/work/project", &accepted()).unwrap();
    assert_eq!(projection.watermark.project_id, "lineage-original");
    assert_eq!(projection.watermark.operation_id, "source-op");
    assert_eq!(projection.watermark.commit_version, 7);
    let saved = json!({"source_publication":{"project_id":"lineage-original",
        "operation_id":"another-operation", "commit_version":7}});
    let error = projection.should_advance(Some(&saved)).unwrap_err();
    assert_eq!(error.code, "invalid_source_projection");
    assert!(error.message.contains("another-operation"));
}
