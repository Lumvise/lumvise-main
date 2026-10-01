use lumvise_mcp_app_adapter::run_lumvise_with_runtime;
use std::fs::File;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const FULL_HELP_CHILD: &str = "LUMVISE_TERMINAL_HELP_FULL_CHILD";

fn assert_help(output: &str, edition: &str) {
    assert!(
        output.contains(edition),
        "missing {edition} edition: {output}"
    );
    assert!(output.contains("start"), "missing start command: {output}");
    assert!(
        output.contains("mcp --project-root"),
        "missing MCP command: {output}"
    );
}

fn isolated_command(command: &mut Command, runtime_root: &std::path::Path) {
    command
        .env_clear()
        .env("LUMVISE_RUNTIME_ROOT", runtime_root)
        .env("LUMVISE_DB_PATH", runtime_root.join("database"))
        .env("LUMVISE_STATE_ROOT", runtime_root.join("state"))
        .env("LUMVISE_PLUGIN_ROOT", runtime_root.join("plugins"))
        .env(
            "LUMVISE_PROVIDER_MODEL_CATALOG_PATH",
            runtime_root.join("models.json"),
        );
    for provider in ["CODEX", "CLAUDE", "GEMINI"] {
        command.env(
            format!("LUMVISE_{provider}_COMMAND"),
            runtime_root.join(format!("no-{}-cli", provider.to_lowercase())),
        );
    }
    #[cfg(unix)]
    command.process_group(0);
}

#[test]
fn community_explicit_help_exits_without_creating_runtime_state() {
    let workspace = tempfile::tempdir().unwrap();
    let runtime_root = workspace.path().join("runtime");

    for flag in ["--help", "-h"] {
        let stdout = workspace.path().join("community.stdout");
        let stderr = workspace.path().join("community.stderr");
        let mut command = Command::new(env!("CARGO_BIN_EXE_lumvise"));
        isolated_command(&mut command, &runtime_root);
        let child = command
            .arg(flag)
            .stdin(Stdio::null())
            .stdout(File::create(&stdout).unwrap())
            .stderr(File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        let mut child = ChildGuard(child);
        let status = child
            .wait_until(Duration::from_secs(8))
            .expect("wait for Community help child");

        assert!(
            status.is_some(),
            "Community {flag} hung instead of showing help"
        );
        assert!(
            status.unwrap().success(),
            "{flag}: {}",
            std::fs::read_to_string(stderr).unwrap()
        );
        assert_help(&std::fs::read_to_string(stdout).unwrap(), "Community");
        assert!(!runtime_root.exists(), "{flag} created runtime state");
    }
}

#[test]
fn full_explicit_help_uses_public_runtime_entrypoint_and_exits_before_startup() {
    let workspace = tempfile::tempdir().unwrap();
    let runtime_root = workspace.path().join("runtime");
    let stdout = workspace.path().join("full.stdout");
    let stderr = workspace.path().join("full.stderr");
    let mut command = Command::new(std::env::current_exe().unwrap());
    isolated_command(&mut command, &runtime_root);
    let output = command
        .args([
            "--exact",
            "full_help_child_calls_public_entrypoint_without_starting_runtime",
            "--nocapture",
        ])
        .env(FULL_HELP_CHILD, "1")
        .stdout(File::create(&stdout).unwrap())
        .stderr(File::create(&stderr).unwrap())
        .spawn()
        .unwrap();
    let mut child = ChildGuard(output);
    let status = child
        .wait_until(Duration::from_secs(8))
        .expect("wait for Full help child");
    assert!(status.is_some(), "Full help child hung instead of exiting");
    assert!(
        status.unwrap().success(),
        "{}",
        std::fs::read_to_string(stderr).unwrap()
    );

    assert_help(&std::fs::read_to_string(stdout).unwrap(), "Full");
    assert!(!runtime_root.exists(), "Full help created runtime state");
}

#[test]
fn full_help_child_calls_public_entrypoint_without_starting_runtime() {
    if std::env::var_os(FULL_HELP_CHILD).is_none() {
        return;
    }
    run_lumvise_with_runtime(vec!["--help".into()], |_| {
        panic!("help must exit before starting the Full runtime")
    })
    .unwrap();
}

struct ChildGuard(Child);

impl ChildGuard {
    fn wait_until(&mut self, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.0.try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            #[cfg(unix)]
            let _ = Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{}", self.0.id())])
                .status();
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn community_no_arguments_shows_help_and_exits_under_a_pty() {
    let workspace = tempfile::tempdir().unwrap();
    let runtime_root = workspace.path().join("runtime");
    let transcript = workspace.path().join("terminal.txt");
    let errors = File::create(workspace.path().join("script.stderr")).unwrap();
    let mut command = Command::new("/usr/bin/script");
    command
        .args([
            "-q",
            transcript.to_str().unwrap(),
            env!("CARGO_BIN_EXE_lumvise"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(errors);
    isolated_command(&mut command, &runtime_root);
    command.env("TERM", "xterm-256color");
    let mut child = ChildGuard(command.spawn().unwrap());
    let status = child
        .wait_until(Duration::from_secs(8))
        .expect("wait for script child");
    assert!(
        status.is_some(),
        "Community CLI hung instead of showing help"
    );
    assert!(status.unwrap().success(), "script exited unsuccessfully");

    let output = std::fs::read_to_string(transcript).unwrap();
    assert_help(&output, "Community");
    assert!(
        !runtime_root.exists(),
        "terminal help created runtime state"
    );
}
