use super::*;

#[test]
fn semantic_codex_jobs_preserve_user_mcp_services_and_only_suppress_completion_notifications() {
    let (_, args) = NativeLlmEngine::Codex.command_and_args(
        Path::new("/project"),
        Path::new("/output.json"),
        "analyze source".into(),
    );
    assert_eq!(&args[..3], ["-c", "notify=[]", "exec"]);
    assert!(args.windows(2).any(|pair| pair == ["-C", "/project"]));
    assert!(!args.iter().any(|arg| arg.contains("mcp_servers")
        || arg.starts_with("features.")
        || arg == "--disable"));
    assert_eq!(args.last().unwrap(), "analyze source");
}

#[test]
fn reconnect_backoff_doubles_to_its_cap_and_logs_each_wait_once() {
    let mut backoff = ReconnectBackoff::new(Duration::from_secs(1), Duration::from_secs(30));
    let steps: Vec<(u64, bool)> = (0..8)
        .map(|_| {
            let (wait, log) = backoff.after_failure();
            (wait.as_secs(), log)
        })
        .collect();
    assert_eq!(
        steps,
        [
            (1, true),
            (2, true),
            (4, true),
            (8, true),
            (16, true),
            (30, true),
            (30, false),
            (30, false),
        ]
    );
    backoff.reset();
    assert_eq!(backoff.after_failure(), (Duration::from_secs(1), true));
}
