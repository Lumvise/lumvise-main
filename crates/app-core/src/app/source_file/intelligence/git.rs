use super::active;
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

pub(super) fn changes(
    root: &Path,
    base: &str,
    control: &InvocationControl,
) -> Result<Value, String> {
    let revision = format!("{base}^{{commit}}");
    let commit = run(
        root,
        &["rev-parse", "--verify", "--end-of-options", &revision],
        control,
    )?;
    let records = run(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-status",
            "--no-renames",
            "--relative",
            "-z",
            commit.trim(),
            "--",
        ],
        control,
    )?;
    let fields: Vec<_> = records.split_terminator('\0').collect();
    let mut changes: Vec<_> = fields
        .chunks_exact(2)
        .map(|pair| json!({"status":pair[0],"path":pair[1]}))
        .collect();
    let untracked = run(
        root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        control,
    )?;
    changes.extend(
        untracked
            .split_terminator('\0')
            .map(|path| json!({"status":"?","path":path})),
    );
    Ok(json!({"base_commit":commit.trim(), "changes":changes}))
}

fn run(root: &Path, args: &[&str], control: &InvocationControl) -> Result<String, String> {
    active(control)?;
    let mut child = Command::new("git")
        .arg("--no-pager")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("git {args:?}: expected executable git: {error}"))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let output = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let status = wait(&mut child, control);
    let bytes = output
        .join()
        .map_err(|_| "git stdout reader panicked")?
        .map_err(|error| error.to_string())?;
    let errors = errors
        .join()
        .map_err(|_| "git stderr reader panicked")?
        .map_err(|error| error.to_string())?;
    if !status?.success() {
        return Err(format!(
            "git {args:?}: expected valid repository/revision: {}",
            String::from_utf8_lossy(&errors)
        ));
    }
    String::from_utf8(bytes).map_err(|error| format!("git {args:?}: expected UTF-8 paths: {error}"))
}

fn wait(
    child: &mut std::process::Child,
    control: &InvocationControl,
) -> Result<std::process::ExitStatus, String> {
    loop {
        if let Err(error) = active(control) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
    }
}
