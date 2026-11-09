//! Test client for the public Start/Continue protocol, without production compatibility paths.
use super::ControlledTestInvoke;
use lumvise_plugin_protocol::WireOutcome;
use lumvise_plugin_runtime::PluginSystem;
use lumvise_plugin_semantic::{PLUGIN_ID, REBUILD_SEARCH_INDEX_EXPORT_ID};
use serde_json::{Value, json};

pub fn run_rebuild(runtime: &PluginSystem, start: Value) -> WireOutcome {
    let root = start["project_root"].clone();
    let mut outcome = runtime
        .invoke(PLUGIN_ID, REBUILD_SEARCH_INDEX_EXPORT_ID, start)
        .expect("rebuild start");
    for _ in 0..100 {
        let WireOutcome::Success { output } = &outcome else {
            return outcome;
        };
        if output["status"] != "pending" {
            return outcome;
        }
        let continuation = json!({"action":"continue","project_root":root,"run_id":output["run_id"],"expected_revision":output["revision"]});
        outcome = runtime
            .invoke(PLUGIN_ID, REBUILD_SEARCH_INDEX_EXPORT_ID, continuation)
            .expect("rebuild continuation");
    }
    panic!("rebuild did not finish after 100 domain continuations");
}
