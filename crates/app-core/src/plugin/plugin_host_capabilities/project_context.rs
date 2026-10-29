//! Shared host project context stays identical between HTTP ingress and plugin calls.
use super::PluginHostServices;
use crate::app::mcp_session_registry::McpSessionMembership;
use std::sync::{Arc, OnceLock};

impl PluginHostServices {
    pub(crate) fn index_publication_client(&self) -> Arc<OnceLock<String>> {
        Arc::clone(&self.index_publication_client)
    }

    pub(crate) fn mcp_session_membership(&self) -> Arc<McpSessionMembership> {
        Arc::clone(&self.mcp_session_membership)
    }
}
