//! Codex approval configuration for the app's trusted Assistant session tools.
//! Both process launch paths call `codex_scoped_tool_approval`; URL rules stay internal.

use crate::llm_providers::LlmMcpServerConfig;

pub(super) fn codex_scoped_tool_approval(server: &LlmMcpServerConfig) -> &'static str {
    if server.name == "lumvise-assistant"
        && url::Url::parse(&server.url).is_ok_and(|url| is_local_assistant_session(&url))
    {
        return "approve";
    }
    "auto"
}

fn is_local_assistant_session(url: &url::Url) -> bool {
    let session = url
        .path()
        .strip_prefix("/api/scoped-plugin-mcp/messages/assistant_session/builtin.assistant/");
    url.scheme() == "http"
        && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && session.is_some_and(|id| !id.is_empty() && !id.contains('/'))
}
