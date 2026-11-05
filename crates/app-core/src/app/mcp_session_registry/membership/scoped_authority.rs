//! Ephemeral provider channels keep their initiating authority across asynchronous LLM turns.
use super::McpSessionMembership;
use lumvise_plugin_runtime::PluginMcpOrigin;

#[derive(Clone)]
pub(crate) struct ScopedMcpTicket {
    pub scope: String,
    pub owner: String,
    pub session: String,
    // Agent runs are never reopened; their run ID supplies the lifecycle fence.
    pub epoch: Option<u64>,
    pub origin: Option<PluginMcpOrigin>,
}

impl McpSessionMembership {
    pub(crate) fn scoped_ticket(&self, requested: ScopedMcpTicket) -> Result<String, String> {
        let mut tickets = self
            .scoped_tickets
            .lock()
            .map_err(|_| "scoped MCP authority lock unavailable")?;
        if let Some((key, _)) = tickets
            .iter()
            .find(|(_, ticket)| same_channel(ticket, &requested))
        {
            return Ok(key.clone());
        }
        if tickets
            .values()
            .any(|ticket| same_session(ticket, &requested) && ticket.epoch > requested.epoch)
        {
            return Err("scoped MCP session; expected its current epoch".into());
        }
        tickets.retain(|_, ticket| {
            !same_session(ticket, &requested) || ticket.epoch == requested.epoch
        });
        let key = uuid::Uuid::new_v4().to_string();
        tickets.insert(key.clone(), requested);
        Ok(key)
    }

    pub(crate) fn resolve_scoped_ticket(&self, key: &str) -> Result<ScopedMcpTicket, String> {
        self.scoped_tickets
            .lock()
            .map_err(|_| "scoped MCP authority lock unavailable")?
            .get(key)
            .cloned()
            .ok_or_else(|| {
                "scoped MCP proof; expected an active host-issued provider channel".into()
            })
    }
}

fn same_channel(left: &ScopedMcpTicket, right: &ScopedMcpTicket) -> bool {
    same_session(left, right) && left.epoch == right.epoch && left.origin == right.origin
}

fn same_session(left: &ScopedMcpTicket, right: &ScopedMcpTicket) -> bool {
    left.session == right.session && left.owner == right.owner && left.scope == right.scope
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immutable_agent_runs_are_distinct_from_reopened_assistant_sessions() {
        let registry = McpSessionMembership::default();
        let agent = ScopedMcpTicket {
            scope: "agent_run".into(),
            owner: "builtin.agents".into(),
            session: "same-visible-id".into(),
            epoch: None,
            origin: None,
        };
        let key = registry.scoped_ticket(agent.clone()).unwrap();
        let assistant = ScopedMcpTicket {
            scope: "assistant_session".into(),
            owner: "builtin.assistant".into(),
            session: "same-visible-id".into(),
            epoch: Some(7),
            origin: None,
        };
        registry.scoped_ticket(assistant.clone()).unwrap();
        let mut reopened = assistant;
        reopened.epoch = Some(8);
        registry.scoped_ticket(reopened).unwrap();
        assert_eq!(
            registry.resolve_scoped_ticket(&key).unwrap().session,
            "same-visible-id"
        );
        let mut another_run = agent;
        another_run.session = "another-run".into();
        assert_ne!(key, registry.scoped_ticket(another_run).unwrap());
        assert_eq!(
            key,
            registry
                .scoped_ticket(registry.resolve_scoped_ticket(&key).unwrap())
                .unwrap()
        );
    }

    #[test]
    fn provider_channel_keeps_its_origin_and_reopen_retires_the_old_epoch() {
        let registry = McpSessionMembership::default();
        let mut channel = ScopedMcpTicket {
            scope: "assistant_session".into(),
            owner: "builtin.assistant".into(),
            session: "workspace:one".into(),
            epoch: Some(4),
            origin: None,
        };
        let native = registry.scoped_ticket(channel.clone()).unwrap();
        assert_eq!(native, registry.scoped_ticket(channel.clone()).unwrap());
        channel.origin = Some(PluginMcpOrigin {
            connector_id: Some("connector:one".into()),
            connection_id: "connection".into(),
            project_epoch: Some(2),
        });
        let external = registry.scoped_ticket(channel.clone()).unwrap();
        assert_ne!(external, native);
        assert_eq!(
            registry.resolve_scoped_ticket(&external).unwrap().origin,
            channel.origin
        );
        channel.epoch = Some(5);
        registry.scoped_ticket(channel).unwrap();
        assert!(registry.resolve_scoped_ticket(&native).is_err());
        assert!(registry.resolve_scoped_ticket(&external).is_err());
        assert!(registry.resolve_scoped_ticket("forged").is_err());
    }

    #[test]
    fn retired_assistant_epoch_cannot_mint_a_new_channel() {
        let registry = McpSessionMembership::default();
        let mut current = ScopedMcpTicket {
            scope: "assistant_session".into(),
            owner: "builtin.assistant".into(),
            session: "workspace:one".into(),
            epoch: Some(7),
            origin: None,
        };
        registry.scoped_ticket(current.clone()).unwrap();
        current.epoch = Some(6);
        assert!(
            registry
                .scoped_ticket(current)
                .unwrap_err()
                .contains("current epoch")
        );
    }
}
