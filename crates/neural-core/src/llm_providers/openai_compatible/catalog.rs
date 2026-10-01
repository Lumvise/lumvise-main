//! Reuses the most recent conversation's validated MCP routes and HTTP pool.
//! The provider owns this bounded cache; failed turns invalidate it without replaying tools.

use crate::error::{NeuralError, Result};
use crate::llm_providers::{LlmMcpServerConfig, LlmRequest, tool_invocation::McpToolCatalog};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(super) struct ConversationToolCatalog {
    cached: Mutex<Option<CachedConversationTools>>,
}

struct CachedConversationTools {
    conversation_id: Option<String>,
    servers: Vec<LlmMcpServerConfig>,
    catalog: Arc<McpToolCatalog>,
}

impl ConversationToolCatalog {
    pub(super) fn resolve(
        &self,
        provider: &str,
        request: &LlmRequest,
    ) -> Result<Arc<McpToolCatalog>> {
        if let Some(cached) = self.cached.lock().map_err(cache_lock_error)?.as_ref()
            && cached.conversation_id == request.conversation_id
            && cached.servers == request.mcp_servers
        {
            tracing::debug!(conversation_id = ?request.conversation_id, "reusing conversation MCP catalog");
            return Ok(Arc::clone(&cached.catalog));
        }
        // Discovery performs network I/O; unrelated conversations must not wait on this lock.
        tracing::debug!(conversation_id = ?request.conversation_id, "discovering conversation MCP catalog");
        let catalog = Arc::new(McpToolCatalog::discover(provider, &request.mcp_servers)?);
        *self.cached.lock().map_err(cache_lock_error)? = Some(CachedConversationTools {
            conversation_id: request.conversation_id.clone(),
            servers: request.mcp_servers.clone(),
            catalog: Arc::clone(&catalog),
        });
        Ok(catalog)
    }

    pub(super) fn invalidate(&self, catalog: &Arc<McpToolCatalog>) {
        if let Ok(mut cached) = self.cached.lock()
            && cached
                .as_ref()
                .is_some_and(|entry| Arc::ptr_eq(&entry.catalog, catalog))
        {
            *cached = None;
        }
    }
}

fn cache_lock_error(error: impl std::fmt::Display) -> NeuralError {
    NeuralError::ProviderFailed {
        provider_id: "mcp".into(),
        message: format!("MCP catalog lock `{error}`; expected available conversation cache"),
    }
}
