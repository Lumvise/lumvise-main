//! Exact private routes for the installed signed memory owners. Host scope checks still
//! authorize every effect; this list grants neither scope nor public discovery.
use lumvise_plugin_package::{ExportDescriptor, ExportSurface};

pub(super) fn permitted(caller: &str, target: &str, export: &ExportDescriptor) -> bool {
    if export.surface != ExportSurface::Command {
        return false;
    }
    matches!(
        (caller, target, export.id.as_str()),
        (
            "builtin.agents",
            "builtin.knowledge",
            "execute_document_delegation" | "prepare_document_maintenance"
        ) | (
            "builtin.knowledge",
            "builtin.agents",
            "prepare_memory_hook_inputs" | "configure_memory_executor"
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumvise_plugin_package::ExecutionMode;
    use serde_json::json;

    fn command(id: &str) -> ExportDescriptor {
        ExportDescriptor {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            surface: ExportSurface::Command,
            input_schema: json!({"type":"object"}),
            output_schema: json!({"type":"object"}),
            admission: None,
            execution: ExecutionMode::Foreground,
        }
    }

    #[test]
    fn memory_command_routes_require_the_exact_installed_owner_pair() {
        for id in [
            "execute_document_delegation",
            "prepare_document_maintenance",
        ] {
            let export = command(id);
            assert!(permitted("builtin.agents", "builtin.knowledge", &export));
            assert!(!permitted("other.caller", "builtin.knowledge", &export));
            assert!(!permitted("builtin.agents", "other.target", &export));
        }
        for id in ["prepare_memory_hook_inputs", "configure_memory_executor"] {
            assert!(permitted(
                "builtin.knowledge",
                "builtin.agents",
                &command(id)
            ));
        }
    }

    #[test]
    fn unrelated_commands_and_scoped_tools_remain_closed() {
        assert!(!permitted(
            "builtin.agents",
            "builtin.knowledge",
            &command("inspect_document_space")
        ));
        let mut scoped = command("prepare_document_maintenance");
        scoped.surface = ExportSurface::ScopedMcpTool {
            scope: "assistant_session".into(),
        };
        assert!(!permitted("builtin.agents", "builtin.knowledge", &scoped));
    }
}
