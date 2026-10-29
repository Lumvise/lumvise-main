use super::*;
use lumvise_plugin_sdk::HostCallTransport;
use serde_json::Value;

struct SourceReadFake {
    output: Value,
    requests: Vec<Value>,
}

impl HostCallTransport for SourceReadFake {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        assert_eq!(capability, "storage.semantic");
        assert_eq!(input["operation"], "revision_families");
        self.requests.push(input);
        Ok(self.output.clone())
    }
}

fn host(remaps: Value) -> SourceReadFake {
    SourceReadFake {
        output: json!({"remaps":remaps}),
        requests: vec![],
    }
}

#[test]
fn reads_explicit_aliases_without_substituting_withdrawn_or_unrequested_ids() {
    let mut host = host(
        json!([{"incoming":{"kind":"element","record_id":"old"},"accepted":{"kind":"element","record_id":"new"}}]),
    );
    let selected = SelectedSources::load(
        &mut PluginContext::for_test(&mut host),
        "/project",
        BTreeSet::from(["old".into(), "withdrawn".into()]),
        Some(9),
    )
    .unwrap();
    assert!(selected.matches("old", "new"));
    assert_eq!(selected.resolve("withdrawn"), None);
    assert_eq!(selected.resolve("already-current"), Some("already-current"));
    assert_eq!(host.requests.len(), 1);
    assert_eq!(host.requests[0]["query"]["expected_commit_version"], 9);
}

#[test]
fn malformed_family_results_fail_instead_of_granting_an_alias() {
    for remaps in [
        json!([{"incoming":{"kind":"element","record_id":"other"},"accepted":{"kind":"element","record_id":"new"}}]),
        json!([{"incoming":{"kind":"element","record_id":"old"},"accepted":{"kind":"artifact","record_id":"new"}}]),
        json!([{"incoming":{"kind":"element","record_id":"old"},"accepted":{"kind":"element","record_id":""}}]),
        json!([{"incoming":{"kind":"element","record_id":"old"},"accepted":{"kind":"element","record_id":"new"}},
            {"incoming":{"kind":"element","record_id":"old"},"accepted":{"kind":"element","record_id":"second"}}]),
    ] {
        let mut host = host(remaps);
        let error = SelectedSources::load(
            &mut PluginContext::for_test(&mut host),
            "/project",
            BTreeSet::from(["old".into()]),
            None,
        )
        .err()
        .unwrap();
        assert_eq!(error.code, "invalid_source_family_read");
        assert!(error.message.contains("expected one selected element"));
    }
}

#[test]
fn empty_requests_do_not_read_storage() {
    let mut host = host(json!([]));
    let selected = SelectedSources::load(
        &mut PluginContext::for_test(&mut host),
        "/project",
        BTreeSet::new(),
        None,
    )
    .unwrap();
    assert!(selected.matches("current", "current"));
    assert!(host.requests.is_empty());
}

#[test]
fn source_requests_include_deleted_workflow_markers_and_typed_references_only() {
    let document = crate::documents::draft_document(serde_json::from_value(json!({"document_id":"context", "project_root":"/project",
        "document_kind":"context", "title":"Context", "content":"", "references":[{"target_kind":"semantic_element","semantic_element_id":"reference"},
        {"target_kind":"artifact","artifact_id":"not-a-source"}], "metadata":{"generation":{"source_element_id":"generated"},
        "provenance":{"semantic_element_id":"provenance"}, "nucleus":{"target_element_id":"nucleus"}, "human":"not-a-source"}})).unwrap());
    let expected = BTreeSet::from([
        "reference".into(),
        "generated".into(),
        "provenance".into(),
        "nucleus".into(),
    ]);
    assert_eq!(document_source_ids(&[document]), expected);
}
