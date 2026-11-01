//! Codex approval configuration for the MCP servers the app presents to a session.
//! Both process launch paths call `codex_scoped_tool_approval`; URL rules stay internal.

use crate::llm_providers::LlmMcpServerConfig;
use url::{Host, Url};

/// Sessions run with `approvalPolicy: never`, so a tool left on `auto` is refused rather than
/// prompted. A server on this machine's loopback interface is a local endpoint (the app's own
/// Assistant and Agent routes included) and gets the standing grant; the address decides, never
/// the server name. Remote and unparsable URLs keep automatic review. The user's own Codex servers
/// never reach this function: the isolated home strips them and the exec launch disables them.
pub(super) fn codex_scoped_tool_approval(server: &LlmMcpServerConfig) -> &'static str {
    if Url::parse(&server.url).is_ok_and(|url| is_loopback_endpoint(&url)) {
        "approve"
    } else {
        "auto"
    }
}

fn is_loopback_endpoint(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
        && match url.host() {
            Some(Host::Domain(name)) => name == "localhost",
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        }
}
