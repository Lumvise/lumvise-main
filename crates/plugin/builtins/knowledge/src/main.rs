use lumvise_plugin_knowledge::KnowledgePlugin;

fn main() {
    if let Err(error) = lumvise_plugin_sdk::run_stdio(&KnowledgePlugin::default()) {
        eprintln!(
            "{}",
            serde_json::json!({
                "event": "knowledge_plugin_exit_error",
                "error": error.to_string(),
            })
        );
        std::process::exit(1);
    }
}
