use lumvise_plugin_semantic::SemanticPlugin;

fn main() {
    if let Err(error) = lumvise_plugin_sdk::run_stdio(&SemanticPlugin) {
        eprintln!(
            "{}",
            serde_json::json!({
                "event": "semantic_plugin_exit_error", "error": error.to_string()
            })
        );
        std::process::exit(1);
    }
}
