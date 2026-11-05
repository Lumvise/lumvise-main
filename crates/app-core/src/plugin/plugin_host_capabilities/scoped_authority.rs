//! Host-only proofs bind provider channels to their signed owner and session or immutable run.
use super::*;
use crate::app::mcp_session_registry::ScopedMcpTicket;

impl PluginHostServices {
    pub(super) fn secure_provider_channels(
        &self,
        plugin: &str,
        capability: &str,
        mut input: Value,
        context: &PluginInvocationContext,
    ) -> Result<Value, HostCapabilityError> {
        let request = match capability {
            NEURAL_LLM => &mut input,
            BACKGROUND_JOB | AUDIO_SESSION => match input.get_mut("request") {
                Some(request) => request,
                None => return Ok(input),
            },
            _ => return Ok(input),
        };
        let Some(servers) = request.get_mut("mcp_servers").and_then(Value::as_array_mut) else {
            return Ok(input);
        };
        let base = self
            .scoped_mcp_base_url
            .lock()
            .map_err(|_| failed(SCOPED_MCP, "MCP URL lock unavailable"))?
            .clone();
        if let Some(base) = base {
            for server in servers {
                self.secure_provider_channel(plugin, &base, server, context)?;
            }
        }
        Ok(input)
    }

    fn secure_provider_channel(
        &self,
        plugin: &str,
        base: &str,
        server: &mut Value,
        context: &PluginInvocationContext,
    ) -> Result<(), HostCapabilityError> {
        let Some(route) = provider_channel_route(base, server) else {
            return Ok(());
        };
        let key = self.provider_channel_key(plugin, route, context)?;
        server["url"] = json!(format!(
            "{}/api/mcp/scoped-authority/{key}/api/scoped-plugin-mcp/messages/{route}",
            base.trim_end_matches('/')
        ));
        Ok(())
    }

    fn provider_channel_key(
        &self,
        plugin: &str,
        route: &str,
        context: &PluginInvocationContext,
    ) -> Result<String, HostCapabilityError> {
        let owns = context
            .plugin_chain()
            .iter()
            .any(|frame| frame.plugin_id == plugin);
        if !owns {
            return Err(invalid(
                SCOPED_MCP,
                &json!(plugin),
                "the signed provider owner in the invocation chain",
            ));
        }
        let ticket = provider_ticket(plugin, route, context)?;
        self.mcp_session_membership
            .scoped_ticket(ticket)
            .map_err(|error| failed(SCOPED_MCP, &error))
    }
}

fn provider_channel_route<'a>(base: &str, server: &'a Value) -> Option<&'a str> {
    let prefix = format!(
        "{}/api/scoped-plugin-mcp/messages/",
        base.trim_end_matches('/')
    );
    server["url"].as_str()?.strip_prefix(&prefix)
}

fn provider_ticket(
    plugin: &str,
    route: &str,
    context: &PluginInvocationContext,
) -> Result<ScopedMcpTicket, HostCapabilityError> {
    let parts = route.split('/').collect::<Vec<_>>();
    let (scope, session, epoch) = provider_route(plugin, &parts)?;
    require_non_empty(SCOPED_MCP, scope, "scope")?;
    require_non_empty(SCOPED_MCP, session, "session or run ID")?;
    Ok(ScopedMcpTicket {
        scope: scope.into(),
        owner: plugin.into(),
        session: session.into(),
        epoch,
        origin: context.mcp_origin().cloned(),
    })
}

fn provider_route<'a>(
    plugin: &str,
    parts: &[&'a str],
) -> Result<(&'a str, &'a str, Option<u64>), HostCapabilityError> {
    match parts {
        // Agent runs never reopen: their existing run ID is the lifecycle fence.
        ["agent_run", "builtin.agents", run] if plugin == "builtin.agents" => {
            Ok(("agent_run", *run, None))
        }
        [scope, owner, session, epoch] if *owner == plugin && *scope != "agent_run" => {
            Ok((*scope, *session, Some(provider_epoch(epoch)?)))
        }
        _ => Err(invalid(
            SCOPED_MCP,
            &json!(parts),
            "a signed provider session with its epoch, or a builtin.agents agent_run",
        )),
    }
}

fn provider_epoch(epoch: &str) -> Result<u64, HostCapabilityError> {
    epoch
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid(SCOPED_MCP, &json!(epoch), "positive session epoch"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumvise_neural_core::llm_providers::{
        LlmCapabilitySupport, LlmProviderCapabilities, LlmResponse,
        contract::{LlmProvider, LlmStreamEventSink},
    };
    use lumvise_neural_core::process::StreamControl;
    use lumvise_plugin_runtime::{PluginInvocationClass, PluginMcpOrigin};
    use std::sync::mpsc::{self, Receiver, Sender};

    struct FakeRunChannelProvider(Sender<LlmRequest>);

    impl LlmProvider for FakeRunChannelProvider {
        fn provider_id(&self) -> &str {
            "cerebras"
        }

        fn capabilities(&self) -> LlmProviderCapabilities {
            LlmProviderCapabilities {
                provider_id: "cerebras".into(),
                final_text_output: LlmCapabilitySupport::Supported,
                streamed_text_output: LlmCapabilitySupport::Unsupported,
                image_snapshot_input: LlmCapabilitySupport::Unsupported,
                live_audio_input: LlmCapabilitySupport::Unsupported,
                screen_frame_broadcast_input: LlmCapabilitySupport::Unsupported,
                native_audio_output: LlmCapabilitySupport::Unsupported,
            }
        }

        fn complete(&self, request: &LlmRequest) -> lumvise_neural_core::Result<LlmResponse> {
            self.0.send(request.clone()).expect("run request receiver");
            Ok(LlmResponse {
                provider_id: "cerebras".into(),
                model: "probe".into(),
                content: "worker received the scoped channel".into(),
                metadata: json!({}),
            })
        }

        fn stream_with_events(
            &self,
            _request: &LlmRequest,
            _control: StreamControl,
            _on_event: &mut LlmStreamEventSink<'_>,
        ) -> lumvise_neural_core::Result<()> {
            panic!("stream request; expected one final Agent completion")
        }
    }

    struct AgentChannelFixture {
        service: Arc<PluginHostServices>,
        requests: Receiver<LlmRequest>,
    }

    impl AgentChannelFixture {
        fn new() -> Self {
            let (sender, requests) = mpsc::channel();
            let registry = LlmProviderRegistry::from_provider_instances(vec![Box::new(
                FakeRunChannelProvider(sender),
            )])
            .unwrap();
            let service = PluginHostServices::new(
                FrontendCore::default(),
                registry,
                Arc::new(lumvise_db_core::LocalPersistence::in_memory().unwrap()),
            );
            *service.scoped_mcp_base_url.lock().unwrap() = Some("http://127.0.0.1:1234".into());
            Self { service, requests }
        }

        fn launch(&self, origin: Option<PluginMcpOrigin>) -> Value {
            let mut context = provider_context("builtin.agents");
            if let Some(origin) = origin {
                context = context.with_mcp_origin(origin);
            }
            self.service
                .invoke_controlled(
                    "builtin.agents",
                    BACKGROUND_JOB,
                    job_request("agent_run/builtin.agents/hook-probe.42"),
                    &context,
                )
                .expect("Agent background job must reach its provider")
        }

        fn provider_ticket(&self, request: &LlmRequest) -> ScopedMcpTicket {
            let url = &request.mcp_servers[0].url;
            let key = url
                .strip_prefix("http://127.0.0.1:1234/api/mcp/scoped-authority/")
                .expect("host-issued channel prefix")
                .split('/')
                .next()
                .unwrap();
            assert!(url.ends_with(
                "/api/scoped-plugin-mcp/messages/agent_run/builtin.agents/hook-probe.42"
            ));
            self.service
                .mcp_session_membership
                .resolve_scoped_ticket(key)
                .unwrap()
        }
    }

    fn provider_context(plugin: &str) -> PluginInvocationContext {
        PluginInvocationContext::new(
            "run",
            "owner",
            PluginInvocationClass::Background,
            Instant::now() + Duration::from_secs(60),
        )
        .enter_plugin(plugin, "agent_tick")
    }

    fn job_request(route: &str) -> Value {
        json!({"operation":"accept_llm", "job_kind":"agent-run:hook-probe",
            "request":{"provider_id":"cerebras","model":"probe",
                "messages":[{"role":"user","content":"Refresh only the test report"}],
                "mcp_servers":[{"name":"lumvise-agent","url":format!(
                    "http://127.0.0.1:1234/api/scoped-plugin-mcp/messages/{route}")}]}})
    }

    #[test]
    fn agent_background_job_reaches_its_provider_with_a_host_issued_run_channel() {
        let fixture = AgentChannelFixture::new();
        let accepted = fixture.launch(None);
        let request = fixture
            .requests
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let ticket = fixture.provider_ticket(&request);
        assert_eq!(
            (
                ticket.scope.as_str(),
                ticket.owner.as_str(),
                ticket.session.as_str()
            ),
            ("agent_run", "builtin.agents", "hook-probe.42")
        );
        assert_eq!(ticket.epoch, None);
        assert_eq!(ticket.origin, None);
        assert_eq!(request.messages[0].content, "Refresh only the test report");
        let completed = fixture
            .service
            .invoke(
                "builtin.agents",
                BACKGROUND_JOB,
                json!({
                    "operation":"wait","job_id":accepted["job_id"],"timeout_ms":1000
                }),
            )
            .unwrap();
        assert_eq!(completed["status"], "completed");
    }

    #[test]
    fn agent_provider_channel_preserves_the_initiating_connector() {
        let fixture = AgentChannelFixture::new();
        let origin = PluginMcpOrigin {
            connector_id: Some("connector:probe".into()),
            connection_id: "probe-connection".into(),
            project_epoch: Some(4),
        };
        fixture.launch(Some(origin.clone()));
        let request = fixture
            .requests
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(fixture.provider_ticket(&request).origin, Some(origin));
        assert_eq!(request.messages[0].content, "Refresh only the test report");
    }

    #[test]
    fn agent_run_support_does_not_relax_session_epoch_or_signed_owner_checks() {
        let fixture = AgentChannelFixture::new();
        for (plugin, route) in [
            (
                "builtin.assistant",
                "assistant_session/builtin.assistant/session",
            ),
            (
                "builtin.assistant",
                "assistant_session/builtin.assistant/session/0",
            ),
            (
                "builtin.assistant",
                "assistant_session/builtin.assistant/session/not-an-epoch",
            ),
            (
                "builtin.assistant",
                "agent_run/builtin.agents/hook-probe.42",
            ),
            (
                "builtin.agents",
                "agent_run/builtin.assistant/hook-probe.42",
            ),
            ("builtin.agents", "agent_run/builtin.agents/hook-probe.42/1"),
            ("builtin.agents", "agent_run/builtin.agents/"),
            ("other.plugin", "agent_run/other.plugin/run"),
        ] {
            let error = fixture
                .service
                .invoke_controlled(
                    plugin,
                    BACKGROUND_JOB,
                    job_request(route),
                    &provider_context(plugin),
                )
                .unwrap_err();
            assert!(
                error.to_string().contains("runtime.scoped_mcp"),
                "route {route} failed outside the channel guard: {error}"
            );
        }
        assert!(fixture.requests.try_recv().is_err());
    }

    #[test]
    fn provider_cannot_mint_an_agent_channel_without_its_signed_owner_frame() {
        let fixture = AgentChannelFixture::new();
        let input = json!({"request":{"mcp_servers":[{"name":"agent","url":
            "http://127.0.0.1:1234/api/scoped-plugin-mcp/messages/agent_run/builtin.agents/hook-probe.42"}]}});
        assert!(
            fixture
                .service
                .secure_provider_channels(
                    "builtin.agents",
                    BACKGROUND_JOB,
                    input,
                    &provider_context("other.plugin")
                )
                .is_err()
        );
    }

    #[test]
    fn unrelated_mcp_servers_keep_their_original_configuration() {
        let fixture = AgentChannelFixture::new();
        let input = json!({"request":{"mcp_servers":[
            {"name":"external","url":"https://example.test/mcp"}
        ]}});
        let output = fixture
            .service
            .secure_provider_channels(
                "builtin.agents",
                BACKGROUND_JOB,
                input.clone(),
                &provider_context("builtin.agents"),
            )
            .unwrap();
        assert_eq!(output, input);
        assert!(fixture.requests.try_recv().is_err());
    }

    #[test]
    fn background_provider_channel_retains_the_initiating_connector_without_changing_the_prompt() {
        let service = PluginHostServices::new(
            FrontendCore::default(),
            LlmProviderRegistry::empty(),
            Arc::new(lumvise_db_core::LocalPersistence::in_memory().unwrap()),
        );
        *service.scoped_mcp_base_url.lock().unwrap() = Some("http://127.0.0.1:1234".into());
        let origin = PluginMcpOrigin {
            connector_id: Some("connector:one".into()),
            connection_id: "connection".into(),
            project_epoch: Some(4),
        };
        let context = PluginInvocationContext::new(
            "job",
            "owner",
            lumvise_plugin_runtime::PluginInvocationClass::Foreground,
            Instant::now() + Duration::from_secs(60),
        )
        .with_mcp_origin(origin.clone())
        .enter_plugin("builtin.assistant", "start_assistant_session");
        let output = service.secure_provider_channels("builtin.assistant", BACKGROUND_JOB, json!({"operation":"accept_llm","request":{
            "messages":[{"role":"user","content":"Keep this request exactly"}],
            "mcp_servers":[{"name":"assistant","url":"http://127.0.0.1:1234/api/scoped-plugin-mcp/messages/assistant_session/builtin.assistant/session/7"}]
        }}), &context).unwrap();
        assert_eq!(
            output["request"]["messages"],
            json!([{"role":"user","content":"Keep this request exactly"}])
        );
        let url = output["request"]["mcp_servers"][0]["url"].as_str().unwrap();
        let key = url
            .strip_prefix("http://127.0.0.1:1234/api/mcp/scoped-authority/")
            .unwrap()
            .split('/')
            .next()
            .unwrap();
        let channel = service
            .mcp_session_membership
            .resolve_scoped_ticket(key)
            .unwrap();
        assert_eq!(channel.origin, Some(origin));
        assert_eq!(channel.session, "session");
        assert_eq!(channel.epoch, Some(7));
    }

    #[test]
    fn status_and_audio_control_requests_do_not_gain_a_spurious_request_field() {
        let service = PluginHostServices::new(
            FrontendCore::default(),
            LlmProviderRegistry::empty(),
            Arc::new(lumvise_db_core::LocalPersistence::in_memory().unwrap()),
        );
        let context = PluginInvocationContext::new(
            "status",
            "owner",
            PluginInvocationClass::Foreground,
            Instant::now() + Duration::from_secs(60),
        );
        for capability in [BACKGROUND_JOB, AUDIO_SESSION] {
            let status = json!({"operation":"status","job_id":"one"});
            assert_eq!(
                service
                    .secure_provider_channels("builtin.assistant", capability, status, &context)
                    .unwrap(),
                json!({"operation":"status","job_id":"one"})
            );
        }
    }
}
