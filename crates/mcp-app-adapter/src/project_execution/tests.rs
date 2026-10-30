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
