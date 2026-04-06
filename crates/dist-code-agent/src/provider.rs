//! Provider implementations for the code-agent distribution.

use kernel_interfaces::provider::{
    ProviderCaps, ProviderError, ProviderInterface, Response, StopReason, Usage,
};
use kernel_interfaces::types::{CompletionConfig, Content, Message, Prompt, Role};

// ============================================================================
// Echo provider (stub for testing without an API key)
// ============================================================================

pub struct EchoProvider;

impl ProviderInterface for EchoProvider {
    fn complete(
        &self,
        prompt: &Prompt,
        _config: &CompletionConfig,
    ) -> Result<Response, ProviderError> {
        let last_user = prompt
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::User))
            .and_then(|m| {
                m.content.iter().find_map(|c| match c {
                    Content::Text(t) => Some(t.clone()),
                    _ => None,
                })
            })
            .unwrap_or_else(|| "(no user input)".into());

        Ok(Response {
            content: vec![Content::Text(format!(
                "[echo provider] You said: {last_user}\n\n\
                 (Set ANTHROPIC_API_KEY to use Claude.)"
            ))],
            usage: Usage {
                input_tokens: last_user.len() / 4,
                output_tokens: 50,
            },
            stop_reason: StopReason::EndTurn,
        })
    }

    fn count_tokens(&self, content: &Content) -> usize {
        match content {
            Content::Text(t) => t.len() / 4,
            Content::ToolCall { input, .. } => input.to_string().len() / 4,
            Content::ToolResult { result, .. } => result.to_string().len() / 4,
        }
    }

    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            supports_tool_use: true,
            supports_vision: false,
            supports_streaming: false,
            max_context_tokens: 200_000,
        }
    }
}

// ============================================================================
// Anthropic provider (real Claude API)
// ============================================================================

pub struct AnthropicProvider {
    client: reqwest::blocking::Client,
    api_key: String,
    model: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
            api_key,
            model,
        }
    }

    /// Convert our Prompt into the Anthropic Messages API request body.
    fn build_request_body(&self, prompt: &Prompt, config: &CompletionConfig) -> serde_json::Value {
        let messages = convert_messages(&prompt.messages);
        let tools = convert_tools(&prompt.tool_definitions);

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": config.max_tokens,
            "messages": messages,
        });

        if !prompt.system.is_empty() {
            body["system"] = serde_json::json!(prompt.system);
        }

        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(tools);
        }

        if let Some(temp) = config.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        if !config.stop_sequences.is_empty() {
            body["stop_sequences"] = serde_json::json!(config.stop_sequences);
        }

        body
    }
}

impl ProviderInterface for AnthropicProvider {
    fn complete(
        &self,
        prompt: &Prompt,
        config: &CompletionConfig,
    ) -> Result<Response, ProviderError> {
        let body = self.build_request_body(prompt, config);

        let http_response = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let status = http_response.status().as_u16();

        if status == 429 {
            let retry_after = http_response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok());
            return Err(ProviderError::RateLimited {
                retry_after_secs: retry_after,
            });
        }

        let response_text = http_response
            .text()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let response_json: serde_json::Value = serde_json::from_str(&response_text)
            .map_err(|e| ProviderError::Parse(format!("{e}: {response_text}")))?;

        if status != 200 {
            let message = response_json
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or(&response_text)
                .to_string();
            return Err(ProviderError::Api { status, message });
        }

        parse_response(&response_json)
    }

    fn count_tokens(&self, content: &Content) -> usize {
        // Approximate: 4 chars per token
        match content {
            Content::Text(t) => t.len() / 4,
            Content::ToolCall { input, .. } => input.to_string().len() / 4,
            Content::ToolResult { result, .. } => result.to_string().len() / 4,
        }
    }

    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            supports_tool_use: true,
            supports_vision: false,
            supports_streaming: false,
            max_context_tokens: 200_000,
        }
    }
}

// ============================================================================
// Conversion helpers
// ============================================================================

/// Convert our Message types to the Anthropic Messages API format.
fn convert_messages(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut result = Vec::new();

    for msg in messages {
        let role = match msg.role {
            Role::User | Role::System => "user",
            Role::Assistant => "assistant",
        };

        let content: Vec<serde_json::Value> = msg
            .content
            .iter()
            .map(|c| match c {
                Content::Text(t) => serde_json::json!({
                    "type": "text",
                    "text": t,
                }),
                Content::ToolCall { id, name, input } => serde_json::json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input,
                }),
                Content::ToolResult { id, result } => serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": result.to_string(),
                }),
            })
            .collect();

        // Anthropic requires alternating user/assistant roles.
        // Merge consecutive same-role messages.
        if let Some(last) = result.last_mut() {
            let last_obj: &mut serde_json::Value = last;
            if last_obj.get("role").and_then(|r| r.as_str()) == Some(role)
                && let Some(arr) = last_obj.get_mut("content").and_then(|c| c.as_array_mut())
            {
                arr.extend(content);
                continue;
            }
        }

        result.push(serde_json::json!({
            "role": role,
            "content": content,
        }));
    }

    result
}

/// Convert our tool definitions to the Anthropic tools format.
fn convert_tools(tool_definitions: &[serde_json::Value]) -> Vec<serde_json::Value> {
    tool_definitions
        .iter()
        .map(|def| {
            // Our format: { "name": ..., "description": ..., "input_schema": ... }
            // Anthropic format: { "name": ..., "description": ..., "input_schema": ... }
            // They match — we just pass them through, ensuring the required fields exist.
            let name = def
                .get("name")
                .cloned()
                .unwrap_or(serde_json::json!("unknown"));
            let description = def
                .get("description")
                .cloned()
                .unwrap_or(serde_json::json!(""));
            let input_schema = def
                .get("input_schema")
                .cloned()
                .unwrap_or(serde_json::json!({
                    "type": "object",
                    "properties": {}
                }));

            serde_json::json!({
                "name": name,
                "description": description,
                "input_schema": input_schema,
            })
        })
        .collect()
}

/// Parse an Anthropic API response into our Response type.
fn parse_response(json: &serde_json::Value) -> Result<Response, ProviderError> {
    let content_blocks = json
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| ProviderError::Parse("missing 'content' array".into()))?;

    let mut content = Vec::new();

    for block in content_blocks {
        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match block_type {
            "text" => {
                let text = block
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                content.push(Content::Text(text));
            }
            "tool_use" => {
                let id = block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let input = block.get("input").cloned().unwrap_or(serde_json::json!({}));
                content.push(Content::ToolCall { id, name, input });
            }
            _ => {
                // Unknown block type — skip
            }
        }
    }

    let stop_reason = match json.get("stop_reason").and_then(|s| s.as_str()) {
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("stop_sequence") => StopReason::StopSequence,
        _ => StopReason::EndTurn,
    };

    let usage = if let Some(u) = json.get("usage") {
        Usage {
            input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
            output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        }
    } else {
        Usage::default()
    };

    Ok(Response {
        content,
        usage,
        stop_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_text_response() {
        let json = serde_json::json!({
            "content": [{"type": "text", "text": "Hello!"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let resp = parse_response(&json).unwrap();
        assert_eq!(resp.content.len(), 1);
        assert!(matches!(&resp.content[0], Content::Text(t) if t == "Hello!"));
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn parse_tool_use_response() {
        let json = serde_json::json!({
            "content": [
                {"type": "text", "text": "Let me read that file."},
                {"type": "tool_use", "id": "toolu_123", "name": "file_read", "input": {"path": "main.rs"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 50, "output_tokens": 30}
        });
        let resp = parse_response(&json).unwrap();
        assert_eq!(resp.content.len(), 2);
        assert!(matches!(&resp.content[0], Content::Text(_)));
        assert!(matches!(&resp.content[1], Content::ToolCall { name, .. } if name == "file_read"));
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn convert_messages_merges_same_role() {
        let messages = vec![
            Message {
                role: Role::User,
                content: vec![Content::Text("Hello".into())],
            },
            Message {
                role: Role::User,
                content: vec![Content::Text("More".into())],
            },
        ];
        let converted = convert_messages(&messages);
        // Should merge into one user message with two text blocks
        assert_eq!(converted.len(), 1);
        let content = converted[0].get("content").unwrap().as_array().unwrap();
        assert_eq!(content.len(), 2);
    }

    #[test]
    fn convert_messages_alternating_roles() {
        let messages = vec![
            Message {
                role: Role::User,
                content: vec![Content::Text("Hello".into())],
            },
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("Hi".into())],
            },
            Message {
                role: Role::User,
                content: vec![Content::Text("Thanks".into())],
            },
        ];
        let converted = convert_messages(&messages);
        assert_eq!(converted.len(), 3);
    }

    #[test]
    fn convert_tools_format() {
        let defs = vec![serde_json::json!({
            "name": "file_read",
            "description": "Read a file",
            "input_schema": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }
        })];
        let tools = convert_tools(&defs);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "file_read");
        assert!(tools[0].get("input_schema").is_some());
    }
}
