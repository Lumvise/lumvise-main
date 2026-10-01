use lumvise_frontend_core::{CanvasPatch, FrontendCore};
use serde_json::json;

#[test]
fn agent_patch_rejects_a_user_revision_created_after_the_agent_read() {
    let mut canvas = FrontendCore::default();
    let agent_revision = canvas.canvas("main").revision;
    canvas
        .update_canvas(
            "main",
            CanvasPatch {
                canvas_id: "main".into(),
                elements: vec![],
            },
        )
        .unwrap();
    let user_scene = canvas.canvas("main").clone();
    let result =
        canvas.apply_canvas_diff("main", json!([]), "plugin:assistant", Some(agent_revision));
    assert!(result.is_err());
    assert_eq!(canvas.canvas("main"), user_scene);
}

#[test]
fn agent_patch_commits_once_at_the_expected_revision() {
    let mut canvas = FrontendCore::default();
    let applied = canvas
        .apply_canvas_diff("main", json!([]), "plugin:assistant", Some(0))
        .unwrap();
    assert_eq!(applied.revision, 1);
    assert!(
        canvas
            .apply_canvas_diff("main", json!([]), "plugin:assistant", Some(0))
            .is_err()
    );
    assert_eq!(canvas.canvas("main").revision, 1);
}
