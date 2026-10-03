//! Server-owned HTTPS routes beside the resource protocol. Authentication and
//! authorization for accepted routes belong to the installed extension.
use lumvise_resource_routing::InvocationControl;

/// Native HTTP request, intentionally without Debug to avoid logging credentials.
pub struct ResourceHttpRequest {
    pub method: String,
    pub path: String,
    pub bearer: Option<String>,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}
/// Complete binary response. Extensions use owned content types and status codes.
pub struct ResourceHttpResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}
/// Optional composition seam for private product endpoints. No extension is
/// installed by the public server. Example: a private hub binds source aliases.
pub trait ResourceHttpExtension: Send + Sync {
    fn accepts(&self, path: &str) -> bool;
    fn handle(
        &self,
        request: ResourceHttpRequest,
        control: &InvocationControl,
    ) -> ResourceHttpResponse;
}
