use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use lumvise_plugin_semantic::SemanticPlugin;
use serde_json::{Value, json};

struct AggregateStorage;
impl HostCallTransport for AggregateStorage {
    fn host_call(&mut self, capability: &str, request: Value) -> Result<Value, PluginError> {
        assert_eq!(capability, "storage.semantic");
        assert_eq!(
            request,
            json!({"operation":"project_element_counts","project_root":"/counts"})
        );
        Ok(
            json!({"commit_version":7,"published_at":"2026-09-22T00:00:00Z","total_elements":100000,"elements_by_kind":{"file":100000}}),
        )
    }
}
#[test]
fn count_export_routes_only_to_graph_aggregation() {
    let mut storage = AggregateStorage;
    let output = SemanticPlugin
        .dispatch(
            "project_element_counts",
            json!({"project_root":"/counts"}),
            &mut PluginContext::for_test(&mut storage),
        )
        .unwrap();
    assert_eq!(output["total_elements"], 100000);
    assert!(output.get("elements").is_none());
    assert!(
        SemanticPlugin
            .dispatch(
                "project_element_counts",
                json!({"project_root":""}),
                &mut PluginContext::for_test(&mut storage)
            )
            .is_err()
    );
}
