//! `runtime.connector_request` through the real broker with real invocation contexts.
use super::tests::request;
use super::*;
use lumvise_contracts::ConnectorCredential;
use lumvise_db_core::LocalPersistence;
use lumvise_plugin_runtime::{PluginInvocationClass, PluginMcpOrigin};
use std::time::{Duration, Instant};

const ASSISTANT: &str = "builtin.assistant";
const CAPABILITY: &str = "runtime.connector_request";

fn broker() -> AppCoreHostCapabilityBroker {
    let persistence = Arc::new(LocalPersistence::in_memory().expect("in-memory persistence"));
    let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
    let relational: Arc<dyn RelationalPersistence> = persistence;
    AppCoreHostCapabilityBroker::new(semantic, relational)
}

fn context() -> PluginInvocationContext {
    PluginInvocationContext::new(
        "request",
        ASSISTANT,
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
}

fn assistant_context(session: &str) -> PluginInvocationContext {
    context()
        .with_route_identity(session, Some("assistant_session".into()))
        .with_scope_epoch(Some(1))
}

fn connector_context(connector: Option<&str>, connection: &str) -> PluginInvocationContext {
    context().with_mcp_origin(PluginMcpOrigin {
        connector_id: connector.map(str::to_owned),
        connection_id: connection.into(),
        project_epoch: None,
    })
}

fn call(
    broker: &AppCoreHostCapabilityBroker,
    context: &PluginInvocationContext,
    input: Value,
) -> Result<Value, HostCapabilityError> {
    broker.invoke_controlled(request(ASSISTANT, CAPABILITY, input), context)
}

/// Registers a connector and binds its MCP connection to it, as the ingress does.
fn connect(
    broker: &AppCoreHostCapabilityBroker,
    credential: char,
    name: &str,
    connection: &str,
) -> String {
    let control = InvocationControl::sixty_seconds();
    let secret = ConnectorCredential(credential.to_string().repeat(64));
    let id = broker
        .knowledge_authority
        .register_connector(&secret, name, &control)
        .expect("register connector")
        .connector_id;
    broker
        .knowledge_authority
        .mcp_membership
        .register_connector(connection, "/project", Some(&id))
        .expect("bind connection to connector");
    id
}

fn bind_session(broker: &AppCoreHostCapabilityBroker, session: &str, active: bool) {
    broker
        .invoke_controlled(
            request(
                ASSISTANT,
                "runtime.knowledge_scope",
                json!({"operation":"bind_assistant","session_id":session,"session_epoch":1,"active":active}),
            ),
            &assistant_context(session),
        )
        .expect("bind Assistant session");
}

fn append(
    broker: &AppCoreHostCapabilityBroker,
    session: &str,
    connector: &str,
    request_text: &str,
) -> Value {
    call(
        broker,
        &assistant_context(session),
        json!({"operation":"append","connector_id":connector,"request":request_text}),
    )
    .expect("append request")["request"]
        .clone()
}

fn list(broker: &AppCoreHostCapabilityBroker, connector: &str, connection: &str) -> Vec<Value> {
    call(
        broker,
        &connector_context(Some(connector), connection),
        json!({"operation":"list"}),
    )
    .expect("list requests")["requests"]
        .as_array()
        .expect("requests array")
        .clone()
}

#[test]
fn append_and_connectors_require_a_bound_live_assistant_session() {
    let broker = broker();
    let connector = connect(&broker, 'a', "Codex", "conn-a");
    let append_input =
        json!({"operation":"append","connector_id":connector,"request":"Check the draft"});

    let unbound = call(&broker, &assistant_context("s1"), append_input.clone()).unwrap_err();
    assert!(unbound.to_string().contains("knowledge_scope_denied"));
    let no_session = call(&broker, &context(), json!({"operation":"connectors"})).unwrap_err();
    assert!(no_session.to_string().contains("knowledge_scope_denied"));

    bind_session(&broker, "s1", true);
    let listed = call(
        &broker,
        &assistant_context("s1"),
        json!({"operation":"connectors"}),
    )
    .expect("bound session lists connectors");
    assert_eq!(
        listed["connectors"],
        json!([{"connector_id": connector, "display_name": "Codex"}])
    );
    call(&broker, &assistant_context("s1"), append_input.clone()).expect("bound session appends");

    bind_session(&broker, "s1", false);
    let terminated = call(&broker, &assistant_context("s1"), append_input).unwrap_err();
    assert!(terminated.to_string().contains("knowledge_scope_denied"));
}

#[test]
fn list_returns_only_the_callers_connector_rows() {
    let broker = broker();
    let connector_a = connect(&broker, 'a', "Codex", "conn-a");
    let connector_b = connect(&broker, 'b', "Claude Code", "conn-b");
    bind_session(&broker, "s1", true);
    let for_a = append(&broker, "s1", &connector_a, "Review the A draft");
    let for_b = append(&broker, "s1", &connector_b, "Review the B draft");

    let rows_a = list(&broker, &connector_a, "conn-a");
    assert_eq!(rows_a.len(), 1);
    assert_eq!(rows_a[0]["request_id"], for_a["request_id"]);
    let rows_b = list(&broker, &connector_b, "conn-b");
    assert_eq!(rows_b.len(), 1);
    assert_eq!(rows_b[0]["request_id"], for_b["request_id"]);
}

#[test]
fn list_and_resolve_without_a_connector_origin_are_denied() {
    let broker = broker();
    let connector = connect(&broker, 'a', "Codex", "conn-a");
    bind_session(&broker, "s1", true);
    append(&broker, "s1", &connector, "Pending");

    for origin in [connector_context(None, "conn-a"), context()] {
        let listed = call(&broker, &origin, json!({"operation":"list"})).unwrap_err();
        assert!(listed.to_string().contains("connector_request_denied"));
        let resolved = call(
            &broker,
            &origin,
            json!({"operation":"resolve","request_id":"x","expected_revision":1,"status":"done"}),
        )
        .unwrap_err();
        assert!(resolved.to_string().contains("connector_request_denied"));
    }
}

#[test]
fn resolve_refuses_stale_revisions_already_resolved_and_foreign_requests() {
    let broker = broker();
    let connector_a = connect(&broker, 'a', "Codex", "conn-a");
    let connector_b = connect(&broker, 'b', "Claude Code", "conn-b");
    bind_session(&broker, "s1", true);
    let request_a = append(&broker, "s1", &connector_a, "Approve the export")["request_id"]
        .as_str()
        .expect("request id")
        .to_owned();
    let as_a = connector_context(Some(&connector_a), "conn-a");
    let resolve = |context: &PluginInvocationContext, revision: u64| {
        call(
            &broker,
            context,
            json!({"operation":"resolve","request_id":request_a,"expected_revision":revision,"status":"done","note":"exported"}),
        )
    };

    assert!(
        resolve(&as_a, 2)
            .unwrap_err()
            .to_string()
            .contains("connector_request_conflict")
    );
    let foreign = call(
        &broker,
        &connector_context(Some(&connector_b), "conn-b"),
        json!({"operation":"resolve","request_id":request_a,"expected_revision":1,"status":"declined"}),
    )
    .unwrap_err();
    assert!(foreign.to_string().contains("connector_request_not_found"));

    let resolved = resolve(&as_a, 1).expect("resolve at current revision");
    assert_eq!(resolved["request"]["status"], "done");
    assert_eq!(resolved["request"]["revision"], 2);
    assert_eq!(resolved["request"]["resolution_note"], "exported");
    assert!(
        resolve(&as_a, 2)
            .unwrap_err()
            .to_string()
            .contains("connector_request_conflict")
    );
    assert!(list(&broker, &connector_a, "conn-a").is_empty());
}

#[test]
fn append_stamps_host_fields_and_rejects_caller_supplied_ones() {
    let broker = broker();
    let connector = connect(&broker, 'a', "Codex", "conn-a");
    bind_session(&broker, "from-context", true);

    for forged in ["origin", "assistant_session_id", "status", "revision"] {
        let mut input = json!({"operation":"append","connector_id":connector,"request":"x"});
        input[forged] = json!("forged");
        let error = call(&broker, &assistant_context("from-context"), input).unwrap_err();
        assert!(
            error.to_string().contains("connector_request_invalid"),
            "{forged}: {error}"
        );
    }

    let stored = append(
        &broker,
        "from-context",
        &connector,
        "First line\nsecond line",
    );
    assert_eq!(stored["origin"], "assistant_request");
    assert_eq!(stored["assistant_session_id"], "from-context");
    assert_eq!(stored["connector_display_name"], "Codex");
    assert_eq!(stored["title"], "First line");
    assert_eq!(stored["status"], "open");
    assert_eq!(stored["revision"], 1);
    assert!(stored["created_at"].as_u64().is_some_and(|value| value > 0));
    assert!(stored.get("resolved_at").is_none());

    let long_title = "t".repeat(201);
    let input =
        json!({"operation":"append","connector_id":connector,"title":long_title,"request":"x"});
    assert!(call(&broker, &assistant_context("from-context"), input).is_err());
    let oversized = "r".repeat(4_001);
    let input = json!({"operation":"append","connector_id":connector,"request":oversized});
    assert!(call(&broker, &assistant_context("from-context"), input).is_err());
}
