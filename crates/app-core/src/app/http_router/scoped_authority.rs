//! Routes only a host-issued provider channel to the exact signed scope it names.
use super::*;

pub(super) fn response(app: &AppCore, request: HttpRequest) -> HttpResponse {
    let Some((key, suffix)) = request
        .path
        .trim_start_matches("/api/mcp/scoped-authority/")
        .split_once('/')
    else {
        return error_response(
            "403 Forbidden",
            "scoped MCP proof; expected an active provider channel",
        );
    };
    let path = format!("/{suffix}");
    let ticket = match app.mcp_session_membership.resolve_scoped_ticket(key) {
        Ok(ticket) => ticket,
        Err(error) => return error_response("403 Forbidden", error),
    };
    let context = parse_scoped_mcp_path(&path, SCOPED_MCP_MESSAGE_ENDPOINT)
        .or_else(|| parse_scoped_mcp_path(&path, SCOPED_MCP_SSE_ENDPOINT));
    let Some(mut context) = context.filter(|context| {
        context.owner_id == ticket.owner
            && context.scope_id == ticket.scope
            && context.session_id == ticket.session
            && context.session_epoch == ticket.epoch
    }) else {
        return error_response(
            "403 Forbidden",
            "scoped MCP route; expected the channel's exact owner, session and epoch",
        );
    };
    context.native_interaction = true;
    context.mcp_origin = ticket.origin;
    match request.method.as_str() {
        "POST" => json_rpc_response(app, request.body, context),
        "GET" if path.starts_with(SCOPED_MCP_SSE_ENDPOINT) => sse_response(&format!(
            "/api/mcp/scoped-authority/{key}{}",
            scoped_message_endpoint(&context)
        )),
        "GET" => streamable_http_get_not_supported(),
        _ => error_response(
            "405 Method Not Allowed",
            "scoped MCP transport; expected GET or POST",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_channel_initializes_only_for_its_exact_owner_scope_and_run() {
        let app = AppCore::in_memory().unwrap();
        let key = app
            .mcp_session_membership
            .scoped_ticket(crate::app::mcp_session_registry::ScopedMcpTicket {
                scope: "agent_run".into(),
                owner: "builtin.agents".into(),
                session: "hook-probe.42".into(),
                epoch: None,
                origin: None,
            })
            .unwrap();
        for route in [
            "agent_run/builtin.agents/other-run",
            "agent_run/other.plugin/hook-probe.42",
            "other_scope/builtin.agents/hook-probe.42",
            "agent_run/builtin.agents/hook-probe.42/1",
        ] {
            assert_eq!(
                response(&app, agent_initialize(&key, route)).status,
                "403 Forbidden"
            );
        }
        let valid = "agent_run/builtin.agents/hook-probe.42";
        assert_eq!(
            response(&app, agent_initialize("forged", valid)).status,
            "403 Forbidden"
        );
        let initialized = response(&app, agent_initialize(&key, valid));
        assert_eq!(initialized.status, "200 OK");
        let body: Value = serde_json::from_slice(initialized.buffered_bytes().unwrap()).unwrap();
        assert_eq!(body["id"], "agent-initialize");
        assert!(body["result"]["capabilities"]["tools"].is_object());
        assert!(body.get("error").is_none());
    }

    fn agent_initialize(key: &str, route: &str) -> HttpRequest {
        HttpRequest {
            native_interaction: false,
            method: "POST".into(),
            path: format!("/api/mcp/scoped-authority/{key}/api/scoped-plugin-mcp/messages/{route}"),
            query: Default::default(),
            authorization: None,
            body: serde_json::to_vec(&serde_json::json!({
                "jsonrpc":"2.0","id":"agent-initialize","method":"initialize",
                "params":{"protocolVersion":"2025-06-18","capabilities":{},
                    "clientInfo":{"name":"agent-probe","version":"1"}}
            }))
            .unwrap(),
        }
    }

    #[test]
    fn scoped_http_requires_the_host_channel_and_refuses_a_different_session_or_epoch() {
        let app = AppCore::in_memory().unwrap();
        let key = app
            .mcp_session_membership
            .scoped_ticket(crate::app::mcp_session_registry::ScopedMcpTicket {
                scope: "assistant_session".into(),
                owner: "builtin.assistant".into(),
                session: "session".into(),
                epoch: Some(7),
                origin: None,
            })
            .unwrap();
        let request = |token: &str, session: &str, epoch: u64| HttpRequest {
            native_interaction: false,
            method: "GET".into(),
            path: format!(
                "/api/mcp/scoped-authority/{token}/api/scoped-plugin-mcp/messages/assistant_session/builtin.assistant/{session}/{epoch}"
            ),
            query: Default::default(),
            authorization: None,
            body: Vec::new(),
        };
        assert_eq!(
            response(&app, request("forged", "session", 7)).status,
            "403 Forbidden"
        );
        assert_eq!(
            response(&app, request(&key, "another", 7)).status,
            "403 Forbidden"
        );
        assert_eq!(
            response(&app, request(&key, "session", 8)).status,
            "403 Forbidden"
        );
        assert_eq!(
            response(&app, request(&key, "session", 7)).status,
            "405 Method Not Allowed"
        );
    }
}
