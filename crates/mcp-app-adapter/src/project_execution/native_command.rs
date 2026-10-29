//! Owns child-process stdin isolation, concurrent output collection and the invocation deadline.
//! The project provider supplies an argument-complete command; MCP input remains broker-owned.
use std::process::{Output, Stdio};
use std::time::Duration;
use tokio::process::{Child, Command};

/// Captures an argument-driven child without sharing MCP stdin.
/// Example: `capture_native_command(command, Duration::from_secs(60)).await`.
pub(super) async fn capture_native_command(
    mut command: Command,
    deadline: Duration,
) -> Result<Output, String> {
    let executable = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    command
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().map_err(|error| {
        format!("failed to start `{executable}`: expected an executable command; {error}")
    })?;
    collect_native_output(child, deadline).await
}

async fn collect_native_output(child: Child, deadline: Duration) -> Result<Output, String> {
    tokio::time::timeout(deadline, child.wait_with_output())
        .await
        .map_err(|_| format!("native command timed out after {}ms", deadline.as_millis()))?
        .map_err(|error| format!("failed to collect native command output: {error}"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    fn shell_command(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    fn capture_fixture(command: Command) -> Result<Output, String> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(capture_native_command(command, Duration::from_millis(800)))
    }

    #[test]
    fn native_commands_cannot_consume_the_callers_protocol_input() {
        let mut input = tempfile::NamedTempFile::new().unwrap();
        writeln!(input, "reserved-mcp-request").unwrap();
        let mut command =
            shell_command("if read value; then printf consumed; else printf closed; fi");
        command.stdin(Stdio::from(File::open(input.path()).unwrap()));
        let output = capture_fixture(command).unwrap();
        assert_eq!(output.stdout, b"closed");
        assert!(output.status.success());
    }

    #[test]
    fn native_commands_drain_both_output_pipes_before_waiting_for_exit() {
        let output = capture_fixture(shell_command(
            "/usr/bin/head -c 262144 /dev/zero; /usr/bin/head -c 262144 /dev/zero >&2",
        ))
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 262144);
        assert_eq!(output.stderr.len(), 262144);
    }

    #[test]
    fn native_command_deadlines_terminate_the_child_process() {
        assert!(
            std::process::Command::new("/bin/kill")
                .args(["-0", &std::process::id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let folder = tempfile::tempdir().unwrap();
        let pid_file = folder.path().join("child.pid");
        let mut command = shell_command("echo $$ > \"$1\"; exec /bin/sleep 30");
        command.arg("deadline-test").arg(&pid_file);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let result = capture_native_command(command, Duration::from_millis(100)).await;
            assert!(result.unwrap_err().contains("timed out after 100ms"));
            tokio::time::sleep(Duration::from_millis(50)).await;
        });
        let pid = std::fs::read_to_string(pid_file).unwrap();
        let alive = std::process::Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!alive, "native child {} survived its deadline", pid.trim());
    }

    #[test]
    fn native_command_spawn_errors_identify_the_missing_executable() {
        let error = capture_fixture(Command::new("/lumvise-missing-native-command")).unwrap_err();
        assert!(error.contains("/lumvise-missing-native-command"));
        assert!(error.contains("expected an executable command"));
    }
}
