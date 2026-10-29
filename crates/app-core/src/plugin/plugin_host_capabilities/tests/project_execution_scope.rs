use super::*;

#[test]
fn project_execution_scope_requires_the_startup_client_identity() {
    let error = services()
        .invoke(
            "builtin.knowledge",
            PROJECT_EXECUTION,
            json!({"operation": "scope"}),
        )
        .expect_err("unbound host must not invent a durable client identity");
    assert!(error.to_string().contains("host_capability_unavailable"));
    assert!(error.to_string().contains("client_instance_id"));
}

#[test]
fn project_execution_scope_reuses_startup_identity_without_an_execution_provider() {
    let services = services();
    crate::index_publication::install_index_client(
        &services.index_publication_client(),
        "client-a".into(),
    )
    .expect("configure startup identity");
    for plugin in ["builtin.knowledge", "plugin.other"] {
        let scope = services
            .invoke(plugin, PROJECT_EXECUTION, json!({"operation": "scope"}))
            .expect("read durable scope without installing an execution provider");
        assert_eq!(scope, json!({"client_instance_id": "client-a"}));
    }
}

#[test]
fn project_execution_scope_distinguishes_independent_instances() {
    for client_id in ["client-a", "client-b"] {
        let services = services();
        crate::index_publication::install_index_client(
            &services.index_publication_client(),
            client_id.into(),
        )
        .expect("configure startup identity");
        let scope = services
            .invoke(
                "builtin.knowledge",
                PROJECT_EXECUTION,
                json!({"operation": "scope"}),
            )
            .expect("read own instance");
        assert_eq!(scope["client_instance_id"], client_id);
    }
}

#[test]
fn project_execution_scope_rejects_a_caller_supplied_identity() {
    let error = services()
        .invoke(
            "builtin.knowledge",
            PROJECT_EXECUTION,
            json!({"operation": "scope", "client_instance_id": "other-client"}),
        )
        .expect_err("scope is owned by startup configuration");
    assert!(error.to_string().contains("invalid_host_capability_input"));
}
