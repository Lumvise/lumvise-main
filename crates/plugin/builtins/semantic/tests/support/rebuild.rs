//! Test client for the public Start/Continue protocol, without production compatibility paths.
use lumvise_plugin_protocol::WireOutcome;
use lumvise_plugin_runtime::{
    PluginInvocationClass, PluginInvocationContext, PluginInvocationRequest, PluginSystem,
};
use lumvise_plugin_semantic::{PLUGIN_ID, REBUILD_SEARCH_INDEX_EXPORT_ID};
use serde_json::{Value, json};

pub fn run_rebuild(runtime: &PluginSystem, start: Value) -> WireOutcome {
    let root = start["project_root"].clone();
    let mut outcome = invoke_rebuild(runtime, start);
    for _ in 0..100 {
        let WireOutcome::Succeeded { value: output } = &outcome else {
            return outcome;
        };
        if output["status"] != "pending" {
            return outcome;
        }
        let continuation = json!({"action":"continue","project_root":root,"run_id":output["run_id"],"expected_revision":output["revision"]});
        outcome = invoke_rebuild(runtime, continuation);
    }
    panic!("rebuild did not finish after 100 domain continuations");
}

fn invoke_rebuild(runtime: &PluginSystem, input: Value) -> WireOutcome {
    // Rebuild clients use the production deadline; ordinary fixture calls retain 3s.
    let context = PluginInvocationContext::new(
        "semantic-rebuild-test",
        "semantic-test-owner",
        PluginInvocationClass::Foreground,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    );
    runtime
        .invoke_controlled(PluginInvocationRequest::new(
            PLUGIN_ID,
            REBUILD_SEARCH_INDEX_EXPORT_ID,
            input,
            context,
        ))
        .expect("controlled rebuild invocation")
}
