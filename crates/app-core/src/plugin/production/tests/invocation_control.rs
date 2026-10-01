use super::*;

#[test]
fn production_policy_preserves_parent_deadline_and_grant_checks() {
    let broker = policy_broker(json!({"schema_version":1,"grants":[
        {"plugin_id":"plugin.alpha","capability_id":"storage.semantic"}]}));
    let context = PluginInvocationContext::new(
        "expired",
        "test",
        lumvise_plugin_runtime::PluginInvocationClass::Foreground,
        std::time::Instant::now() - std::time::Duration::from_secs(1),
    );
    let mut request = storage_request("plugin.alpha", "^1.3");
    request.capability_id = "storage.semantic".into();
    request.input = json!({"operation":"project_element_counts","project_root":"/test"});
    let error = broker
        .invoke_controlled(request.clone(), &context)
        .unwrap_err();
    assert!(error.to_string().contains("deadline"), "{error}");
    request.plugin_id = "ungranted".into();
    let error = broker.invoke_controlled(request, &context).unwrap_err();
    assert!(
        error.to_string().contains("host_capability_denied"),
        "{error}"
    );
}
