//! Product executable role selection for runtime and per-client MCP operation.
//!
//! External launchers call `run_lumvise` or `run_lumvise_with_runtime`; parsing and role-specific startup
//! remain private so the product cannot grow parallel executable contracts.

use crate::{
    AppBridgeConfig, McpAppConfig, ProjectExecutionConfig, ProjectExecutionProvider, open_server,
};
use lumvise_app_core::{AcquireResult, ActivationRequest, AppRuntimeCoordinator, OwnerLease};
use lumvise_mcp_core::run_stdio;
use std::io::IsTerminal;

/// Leading runtime-role flag used by MCP brokers; never foregrounds a running app.
pub(crate) const BACKGROUND_LAUNCH_FLAG: &str = "--background-launch";

/// Runs the Community CLI with the headless runtime owner and terminal help.
///
/// # Example
/// ```no_run
/// lumvise_mcp_app_adapter::run_lumvise(std::env::args().skip(1).collect())?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn run_lumvise(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    run_product(
        args,
        ProductEdition::Community,
        lumvise_app_core::run_headless_app,
    )
}

/// Runs the Full CLI with a product-owned runtime launcher and terminal help.
///
/// # Example
/// ```ignore
/// run_lumvise_with_runtime(arguments, |owner| desktop_runtime(owner))?;
/// ```
pub fn run_lumvise_with_runtime(
    args: Vec<String>,
    run_owner: impl FnOnce(OwnerLease) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    run_product(args, ProductEdition::Full, run_owner)
}

fn run_product(
    args: Vec<String>,
    edition: ProductEdition,
    run_owner: impl FnOnce(OwnerLease) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let terminal = std::io::stdin().is_terminal() || std::io::stdout().is_terminal();
    match ProductInvocation::parse(args, terminal) {
        ProductInvocation::Help => {
            print_help(edition);
            Ok(())
        }
        ProductInvocation::Runtime {
            arguments,
            background,
        } => run_runtime(arguments, background, run_owner),
        ProductInvocation::Mcp(options) => run_mcp(options),
    }
}

enum ProductEdition {
    Community,
    Full,
}

fn print_help(edition: ProductEdition) {
    let (name, description) = match edition {
        ProductEdition::Community => ("Community", "Headless runtime, MCP, graphs, and knowledge."),
        ProductEdition::Full => ("Full", "Desktop workspace, Assistant, canvases, and MCP."),
    };
    println!(
        "Lumvise {name} {}\n{description}\n\n\
         Usage: lumvise <command>\n\n\
         Commands:\n  start                         Start Lumvise\n  \
         mcp --project-root <path>     Connect an MCP client to a project\n  \
         --help, -h                    Show this help\n\n\
         MCP options: --runtime-root <path>, --instance-id <id>, --native-llm-engine <name>\n\
         Learn more: https://github.com/Lumvise/lumvise-main",
        env!("CARGO_PKG_VERSION")
    );
}

#[derive(Debug, PartialEq, Eq)]
enum ProductInvocation {
    Help,
    Runtime {
        arguments: Vec<String>,
        background: bool,
    },
    Mcp(McpOptions),
}

#[derive(Debug, Default, PartialEq, Eq)]
struct McpOptions {
    runtime_root: Option<String>,
    project_root: Option<String>,
    instance_id: Option<String>,
    native_llm_engine: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct McpIdentity {
    engine: String,
    instance_id: String,
}

impl ProductInvocation {
    fn parse(args: Vec<String>, terminal: bool) -> Self {
        match args.first().map(String::as_str) {
            None if terminal => Self::Help,
            Some("--help" | "-h" | "help") => Self::Help,
            Some("start") => Self::Runtime {
                arguments: args[1..].to_vec(),
                background: false,
            },
            Some("mcp") => Self::Mcp(McpOptions::parse(&args[1..])),
            Some(BACKGROUND_LAUNCH_FLAG) => Self::Runtime {
                arguments: args[1..].to_vec(),
                background: true,
            },
            _ => Self::Runtime {
                arguments: args,
                background: false,
            },
        }
    }
}

impl McpOptions {
    fn parse(args: &[String]) -> Self {
        Self {
            runtime_root: value_after(args, "--runtime-root"),
            project_root: value_after(args, "--project-root"),
            instance_id: value_after(args, "--instance-id"),
            native_llm_engine: value_after(args, "--native-llm-engine"),
        }
    }

    fn app_bridge(&self) -> AppBridgeConfig {
        match self.runtime_root.as_deref() {
            Some(root) => AppBridgeConfig::discovery().with_runtime_root(root),
            None => AppBridgeConfig::discovery(),
        }
    }

    fn identity(&self) -> McpIdentity {
        McpIdentity {
            engine: self
                .native_llm_engine
                .clone()
                .or_else(|| std::env::var("LUMVISE_NATIVE_LLM_ENGINE").ok())
                .unwrap_or_else(|| "codex".into()),
            instance_id: self
                .instance_id
                .clone()
                .unwrap_or_else(|| format!("lumvise-mcp-{}", std::process::id())),
        }
    }

    fn project_provider(
        &self,
        app_bridge: &AppBridgeConfig,
        identity: &McpIdentity,
    ) -> Result<Option<ProjectExecutionProvider>, String> {
        let Some(project_root) = self.project_root.as_deref() else {
            return Ok(None);
        };
        ProjectExecutionProvider::spawn(
            ProjectExecutionConfig::new(app_bridge.clone(), &identity.instance_id, project_root)
                .with_native_llm_engine(&identity.engine),
        )
        .map(Some)
    }
}

fn run_mcp(options: McpOptions) -> Result<(), Box<dyn std::error::Error>> {
    let app_bridge = options.app_bridge();
    let identity = options.identity();
    let _project_provider = options.project_provider(&app_bridge, &identity)?;
    let server = open_server(
        McpAppConfig::from_app_bridge(app_bridge)
            .with_native_assistant_caller(identity.engine, identity.instance_id),
    );
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    run_stdio(server, stdin.lock(), stdout)?;
    Ok(())
}

fn run_runtime(
    arguments: Vec<String>,
    background: bool,
    run_owner: impl FnOnce(OwnerLease) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let coordinator = AppRuntimeCoordinator::production();
    let request = ActivationRequest {
        arguments,
        background,
    };
    match coordinator.acquire_or_forward(request)? {
        AcquireResult::Owner(owner) => run_owner(owner),
        AcquireResult::Forwarded(_) => Ok(()),
    }
}

fn value_after(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_terminal_launch_shows_help_but_nonterminal_launch_starts_runtime() {
        assert_eq!(
            ProductInvocation::parse(vec![], true),
            ProductInvocation::Help
        );
        assert_eq!(
            ProductInvocation::parse(vec![], false),
            ProductInvocation::Runtime {
                arguments: vec![],
                background: false,
            }
        );
    }

    #[test]
    fn explicit_start_preserves_activation_arguments_in_a_terminal() {
        assert_eq!(
            ProductInvocation::parse(
                vec!["start".into(), "--file".into(), "note.md".into()],
                true
            ),
            ProductInvocation::Runtime {
                arguments: vec!["--file".into(), "note.md".into()],
                background: false,
            },
        );
    }

    #[test]
    fn default_role_preserves_runtime_activation_arguments() {
        let arguments = vec!["--file".into(), "note.md".into()];
        assert_eq!(
            ProductInvocation::parse(arguments.clone(), false),
            ProductInvocation::Runtime {
                arguments,
                background: false,
            }
        );
    }

    #[test]
    fn background_launch_flag_selects_runtime_without_foreground_activation() {
        assert_eq!(
            ProductInvocation::parse(vec![BACKGROUND_LAUNCH_FLAG.into()], true),
            ProductInvocation::Runtime {
                arguments: Vec::new(),
                background: true,
            }
        );
    }

    #[test]
    fn explicit_mcp_role_parses_runtime_and_project_scope() {
        let invocation = ProductInvocation::parse(
            vec![
                "mcp".into(),
                "--runtime-root".into(),
                "/runtime".into(),
                "--project-root".into(),
                "/project".into(),
                "--instance-id".into(),
                "client-1".into(),
                "--native-llm-engine".into(),
                "codex".into(),
            ],
            true,
        );
        assert_eq!(
            invocation,
            ProductInvocation::Mcp(McpOptions {
                runtime_root: Some("/runtime".into()),
                project_root: Some("/project".into()),
                instance_id: Some("client-1".into()),
                native_llm_engine: Some("codex".into()),
            })
        );
    }
}

#[cfg(test)]
mod entrypoint_tests {
    use super::*;
    use lumvise_app_core::{QuitRequest, RuntimeControlPort};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingRuntimeControl {
        arguments: Mutex<Vec<Vec<String>>>,
    }

    impl RuntimeControlPort for RecordingRuntimeControl {
        fn activate(&self, arguments: Vec<String>) -> Result<(), String> {
            self.arguments.lock().unwrap().push(arguments);
            Ok(())
        }
        fn quit(&self) -> Result<(), String> {
            Ok(())
        }
    }

    fn assert_isolated_runtime_entrypoint() {
        let root = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env_clear()
            .args(["--exact", "product::entrypoint_tests::injected_runtime_preserves_forwarding_background_and_launch_errors", "--nocapture"])
            .env("LUMVISE_ENTRYPOINT_TEST_CHILD", "1")
            .env("LUMVISE_RUNTIME_ROOT", root.path())
            .env("LUMVISE_ROUTE_GRAPH_PERSISTENCE", "invalid-test-placement")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn reject_owner_after_forwarding(
        mut owner: OwnerLease,
        control: Arc<RecordingRuntimeControl>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        owner.install_control_port(control)?;
        owner.mark_ready("http://127.0.0.1:61235")?;
        run_lumvise_with_runtime(vec!["--file".into(), "note.md".into()], |_| {
            panic!("forwarded launch must not acquire ownership")
        })?;
        run_lumvise_with_runtime(
            vec!["start".into(), "--file".into(), "other.md".into()],
            |_| panic!("explicit start must forward to the current owner"),
        )?;
        run_lumvise_with_runtime(vec![BACKGROUND_LAUNCH_FLAG.into()], |_| {
            panic!("background join must not acquire ownership")
        })?;
        owner.begin_quit(QuitRequest::default())?;
        Err(std::io::Error::other("fake owner launcher failed").into())
    }

    fn assert_runtime_forwarding_and_failure() {
        let control = Arc::new(RecordingRuntimeControl::default());
        let error = run_lumvise_with_runtime(vec![], |owner| {
            reject_owner_after_forwarding(owner, control.clone())
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "fake owner launcher failed");
        assert_eq!(
            *control.arguments.lock().unwrap(),
            vec![
                vec!["--file".to_string(), "note.md".to_string()],
                vec!["--file".to_string(), "other.md".to_string()],
            ]
        );
        let headless_error = run_lumvise(vec![]).unwrap_err();
        assert!(
            headless_error
                .to_string()
                .starts_with("reading resource routing configuration:"),
            "{headless_error}"
        );
    }

    #[test]
    fn injected_runtime_preserves_forwarding_background_and_launch_errors() {
        if std::env::var_os("LUMVISE_ENTRYPOINT_TEST_CHILD").is_none() {
            assert_isolated_runtime_entrypoint();
            return;
        }
        assert_runtime_forwarding_and_failure();
    }
}
