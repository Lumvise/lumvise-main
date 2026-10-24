use std::sync::Arc;

use lumvise_mcp_core::{
    LumviseMcpServer, McpApplication, McpApplicationError, McpTool, encode_mcp_tool_result,
};
use serde_json::{Value, json};

struct RenderedCanvasFakeApplication {
    result: Value,
}

impl McpApplication for RenderedCanvasFakeApplication {
    fn list_tools(&self) -> Result<Vec<McpTool>, McpApplicationError> {
        Ok(vec![McpTool::new(
            "canvas.preview",
            "Rendered PNG",
            json!({"type":"object"}),
        )])
    }

    fn invoke_tool(&self, _name: &str, _arguments: Value) -> Result<Value, McpApplicationError> {
        Ok(self.result.clone())
    }
}

#[test]
fn json_rpc_preserves_rendered_images_across_both_mcp_encoding_boundaries() {
    let image = json!({"type":"image","mimeType":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII="});
    let preview = json!({"content":[image.clone()],"structuredContent":{"revision":7}});
    for result in [preview.clone(), encode_mcp_tool_result(preview, false)] {
        let server = LumviseMcpServer::new(Arc::new(RenderedCanvasFakeApplication { result }));
        let response = server.handle_json_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"canvas.preview","arguments":{}}}"#,
        ).unwrap().unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["result"]["content"][0], image);
        assert_eq!(response["result"]["structuredContent"]["revision"], 7);
        assert_eq!(response["result"]["isError"], false);
    }
}
