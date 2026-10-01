use super::McpTool;
use serde_json::{Value, json};

pub fn to_openai_function_tool(tool: &McpTool) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.input_schema,
    })
}

pub fn to_openai_chat_function_tool(tool: &McpTool) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.input_schema,
        }
    })
}

pub fn to_gemini_declaration(tool: &McpTool) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.input_schema,
    })
}

pub fn to_anthropic_tool(tool: &McpTool) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.input_schema,
    })
}
