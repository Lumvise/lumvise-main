use super::*;
use lumvise_db_core::{SemanticOperation, SemanticReadiness, SemanticResult};

struct FakeProjectRoots {
    root: String,
}
impl SemanticPersistence for FakeProjectRoots {
    fn execute(
        &self,
        operation: SemanticOperation,
        _control: &InvocationControl,
    ) -> lumvise_db_core::Result<SemanticResult> {
        assert!(matches!(operation, SemanticOperation::ProjectRoots));
        Ok(SemanticResult::ProjectRoots(vec![self.root.clone()]))
    }
    fn readiness(&self) -> lumvise_db_core::Result<SemanticReadiness> {
        Ok(SemanticReadiness { ready: true })
    }
}

fn invoke(root: &Path, mut input: Value) -> Result<Value, String> {
    let root = root.to_string_lossy().into_owned();
    input["project_root"] = json!(root);
    invoke_project_source(
        &FakeProjectRoots { root },
        input,
        &InvocationControl::sixty_seconds(),
    )
}

#[test]
fn reads_ranges_and_searches_regex_with_explicit_unreadable_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("sample.rs"),
        "fn alpha() {}\nfn beta() {}\n",
    )
    .unwrap();
    let read = invoke(
        dir.path(),
        json!({"operation":"read","path":"sample.rs","start_line":2,"end_line":9}),
    )
    .unwrap();
    assert_eq!(read["text"], "fn beta() {}");
    assert_eq!(read["end_line"], 2);
    let search=invoke(dir.path(),json!({"operation":"search","paths":["sample.rs","missing.rs"],"pattern":"ALPHA|BETA","regex":true,"case_sensitive":false})).unwrap();
    assert_eq!(search["matches"].as_array().unwrap().len(), 2);
    assert_eq!(search["failures"].as_array().unwrap().len(), 1);
    assert!(
        invoke(
            dir.path(),
            json!({"operation":"read","path":"sample.rs","start_line":0,"end_line":1})
        )
        .is_err()
    );
}

#[test]
fn source_operations_reject_unindexed_roots_escape_symlinks_and_expired_calls() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), "outside").unwrap();
    let input = json!({"operation":"read","path":outside.path().join("secret"),"start_line":1,"end_line":1});
    assert!(invoke(dir.path(), input).unwrap_err().contains("outside"));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("link")).unwrap();
        assert!(
            invoke(
                dir.path(),
                json!({"operation":"read","path":"link","start_line":1,"end_line":1})
            )
            .is_err()
        );
    }
    let request = json!({"operation":"git_changes","project_root":dir.path(),"base":"HEAD"});
    assert!(
        invoke_project_source(
            &FakeProjectRoots {
                root: outside.path().to_string_lossy().into()
            },
            request.clone(),
            &InvocationControl::sixty_seconds()
        )
        .unwrap_err()
        .contains("not an indexed project")
    );
    assert!(
        invoke_project_source(
            &FakeProjectRoots {
                root: dir.path().to_string_lossy().into()
            },
            request,
            &InvocationControl::with_deadline(std::time::Duration::ZERO)
        )
        .unwrap_err()
        .contains("deadline")
    );
}

#[test]
fn git_impact_reads_tracked_deleted_and_untracked_paths_without_shell_interpolation() {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(dir.path().join("old.rs"), "fn old() {}\n").unwrap();
    git(&["add", "old.rs"]);
    git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-qm",
        "fixture",
    ]);
    std::fs::remove_file(dir.path().join("old.rs")).unwrap();
    std::fs::write(dir.path().join("new file.rs"), "new").unwrap();
    let changes = invoke(dir.path(), json!({"operation":"git_changes","base":"HEAD"})).unwrap();
    assert_eq!(
        changes["changes"],
        json!([{"path":"old.rs","status":"D"},{"path":"new file.rs","status":"?"}])
    );
    assert!(
        invoke(
            dir.path(),
            json!({"operation":"git_changes","base":"--output=/tmp/invalid"})
        )
        .is_err()
    );
}
