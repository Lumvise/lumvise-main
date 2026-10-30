use super::*;
use std::path::PathBuf;
use std::time::Instant;

struct NativeProcessForest {
    folder: tempfile::TempDir,
}

struct SharedProcessService(std::process::Child);

impl SharedProcessService {
    fn start() -> Self {
        Self(
            std::process::Command::new("/bin/sleep")
                .arg("30")
                .spawn()
                .unwrap(),
        )
    }
}

impl Drop for SharedProcessService {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl NativeProcessForest {
    fn new() -> Self {
        Self {
            folder: tempfile::tempdir().unwrap(),
        }
    }

    fn pid_path(&self) -> PathBuf {
        self.folder.path().join("descendants.pid")
    }

    fn command(&self, ending: &str, inherit_pipes: bool) -> Command {
        let redirect = if inherit_pipes {
            ""
        } else {
            "</dev/null >/dev/null 2>&1"
        };
        let script = format!(
            "sh -c 'sleep 30 & echo \"$$ $!\" > \"$1\"; wait' forest \"$1\" {redirect} & \
             while [ ! -s \"$1\" ]; do sleep 0.01; done; printf captured; {ending}"
        );
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &script, "forest"]).arg(self.pid_path());
        command
    }

    async fn wait_until_started(&self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.pids().len() != 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fixture descendants must start");
    }

    fn pids(&self) -> Vec<String> {
        std::fs::read_to_string(self.pid_path())
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }

    async fn assert_stopped(&self) {
        let pids = self.pids();
        assert_eq!(pids.len(), 2, "expected child and grandchild fixture PIDs");
        let deadline = Instant::now() + Duration::from_secs(2);
        while pids.iter().any(|pid| process_running(pid)) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            pids.iter().all(|pid| !process_running(pid)),
            "native descendants survived: {pids:?}"
        );
    }
}

impl Drop for NativeProcessForest {
    fn drop(&mut self) {
        for pid in self.pids() {
            let _ = std::process::Command::new("/bin/kill")
                .args(["-KILL", &pid])
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn process_running(pid: &str) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&output.stdout);
    !state.trim().is_empty() && !state.trim().starts_with('Z')
}

fn run_fixture(test: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}

#[test]
fn native_success_closes_descendants_even_when_they_hold_output_pipes() {
    run_fixture(async {
        let forest = NativeProcessForest::new();
        let output = capture_native_command(forest.command("exit 0", true), Duration::from_secs(5))
            .await
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"captured");
        forest.assert_stopped().await;
    });
}

#[test]
fn native_failure_closes_descendants_with_redirected_output() {
    run_fixture(async {
        let forest = NativeProcessForest::new();
        let output =
            capture_native_command(forest.command("exit 7", false), Duration::from_secs(5))
                .await
                .unwrap();
        forest.assert_stopped().await;
        assert_eq!(output.status.code(), Some(7));
    });
}

#[test]
fn native_timeout_closes_the_entire_process_tree() {
    run_fixture(async {
        let forest = NativeProcessForest::new();
        let output =
            capture_native_command(forest.command("wait", false), Duration::from_millis(500)).await;
        assert!(output.unwrap_err().contains("timed out"));
        forest.assert_stopped().await;
    });
}

#[test]
fn dropping_a_native_job_closes_its_entire_process_tree() {
    run_fixture(async {
        let forest = NativeProcessForest::new();
        let task = tokio::spawn(capture_native_command(
            forest.command("wait", false),
            Duration::from_secs(30),
        ));
        forest.wait_until_started().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        forest.assert_stopped().await;
    });
}

#[test]
fn successive_native_jobs_leave_no_descendants_behind() {
    run_fixture(async {
        for _ in 0..5 {
            let forest = NativeProcessForest::new();
            capture_native_command(forest.command("exit 0", false), Duration::from_secs(5))
                .await
                .unwrap();
            forest.assert_stopped().await;
        }
    });
}

#[test]
fn native_cleanup_preserves_shared_services_outside_the_job_tree() {
    run_fixture(async {
        let mut shared = SharedProcessService::start();
        let forest = NativeProcessForest::new();
        capture_native_command(forest.command("exit 0", false), Duration::from_secs(5))
            .await
            .unwrap();
        forest.assert_stopped().await;
        assert!(
            shared.0.try_wait().unwrap().is_none(),
            "cleanup killed a shared service"
        );
    });
}

#[test]
fn native_completion_allows_descendants_to_handle_termination() {
    run_fixture(async {
        let forest = NativeProcessForest::new();
        let marker = forest.folder.path().join("closed");
        let mut command = Command::new("/bin/sh");
        let parent =
            r#"sh -c "$2" child "$1" "$3" & while [ ! -s "$1" ]; do sleep 0.01; done; exit 0"#;
        let descendant = r#"trap 'echo closed > "$2"; exit 0' TERM; echo $$ > "$1"; while :; do sleep 0.05; done"#;
        command
            .args(["-c", parent, "parent"])
            .arg(forest.pid_path())
            .arg(descendant)
            .arg(&marker);
        capture_native_command(command, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(marker).unwrap().trim(), "closed");
    });
}

#[test]
fn native_cleanup_forces_descendants_that_ignore_termination() {
    run_fixture(async {
        let forest = NativeProcessForest::new();
        let script = r#"sh -c "$2" child "$1" & while [ ! -s "$1" ]; do sleep 0.01; done; exit 0"#;
        let descendant = r#"trap '' TERM; sleep 30 & echo "$$ $!" > "$1"; wait"#;
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", script, "parent"])
            .arg(forest.pid_path())
            .arg(descendant);
        let started = Instant::now();
        capture_native_command(command, Duration::from_secs(5))
            .await
            .unwrap();
        forest.assert_stopped().await;
        assert!(started.elapsed() < Duration::from_secs(3));
    });
}
