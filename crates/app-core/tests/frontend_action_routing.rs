use lumvise_app_core::AppCore;
use serde_json::json;

#[test]
fn windows_cannot_consume_each_others_actions() {
    let app = AppCore::in_memory().unwrap();
    app.enqueue_frontend_action(json!({"action":"frontend.start_countdown"}))
        .unwrap();
    app.enqueue_frontend_action(json!({"action":"workspace.navigate", "payload":{
        "target_window":"lumvise-workspace", "projectRoot":"/project"
    }}))
    .unwrap();
    assert!(
        app.drain_window_actions("lumvise-settings")
            .unwrap()
            .is_empty()
    );
    let dashboard = app.drain_window_actions("lumvise-frontend").unwrap();
    assert_eq!(dashboard.len(), 1);
    assert_eq!(dashboard[0]["action"], "frontend.start_countdown");
    let workspace = app.drain_window_actions("lumvise-workspace").unwrap();
    assert_eq!(workspace.len(), 1);
    assert_eq!(workspace[0]["action"], "workspace.navigate");
    assert!(app.drain_frontend_actions().unwrap().is_empty());
}
