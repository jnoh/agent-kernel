//! Provider implementations for the kernel daemon.
//!
//! The daemon owns the model — it calls the LLM API directly.
//! Distros never interact with the provider.

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
                ..Default::default()
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
    api_key: String,
    model: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self { api_key, model }
    }

    fn cache_min_tokens(&self) -> usize {
        if self.model.contains("sonnet") {
            2048
        } else {
            4096
        }
    }

    fn build_request_body(&self, prompt: &Prompt, config: &CompletionConfig) -> serde_json::Value {
        let mut messages = convert_messages(&prompt.messages);
        let tools = convert_tools(&prompt.tool_definitions);

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": config.max_tokens,
            "messages": messages,
        });

        let system_chars = prompt.system.len();
        let tools_chars: usize = tools.iter().map(|t| t.to_string().len()).sum();
        let msgs_chars: usize = messages.iter().map(|m| m.to_string().len()).sum();
        let total_prefix_tokens = (system_chars + tools_chars + msgs_chars) / 4;
        let min_tokens = self.cache_min_tokens();

        if total_prefix_tokens >= min_tokens {
            if !prompt.system.is_empty() {
                body["system"] = serde_json::json!([{
                    "type": "text",
                    "text": prompt.system,
                }]);
            }

            if !tools.is_empty() {
                let mut tools = tools;
                if let Some(last_tool) = tools.last_mut() {
                    last_tool["cache_control"] =
                        serde_json::json!({ "type": "ephemeral", "ttl": "1h" });
                }
                body["tools"] = serde_json::Value::Array(tools);
            } else if !prompt.system.is_empty() {
                body["system"] = serde_json::json!([{
                    "type": "text",
                    "text": prompt.system,
                    "cache_control": { "type": "ephemeral", "ttl": "1h" }
                }]);
            }

            if messages.len() >= 2 {
                let cache_idx = messages.len() - 2;
                if let Some(msg) = messages.get_mut(cache_idx)
                    && let Some(content_arr) = msg.get_mut("content").and_then(|c| c.as_array_mut())
                    && let Some(last_block) = content_arr.last_mut()
                {
                    last_block["cache_control"] = serde_json::json!({ "type": "ephemeral" });
                }
                body["messages"] = serde_json::Value::Array(messages);
            }
        } else {
            if !prompt.system.is_empty() {
                body["system"] = serde_json::json!(prompt.system);
            }
            if !tools.is_empty() {
                body["tools"] = serde_json::Value::Array(tools);
            }
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
        let body_str = serde_json::to_string(&body)
            .map_err(|e| ProviderError::Parse(format!("failed to serialize request: {e}")))?;

        let response = ureq::post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .send(body_str.as_bytes());

        match response {
            Ok(mut resp) => {
                let response_text = resp
                    .body_mut()
                    .read_to_string()
                    .map_err(|e| ProviderError::Network(format!("failed to read body: {e}")))?;
                let response_json: serde_json::Value = serde_json::from_str(&response_text)
                    .map_err(|e| ProviderError::Parse(format!("{e}: {response_text}")))?;
                parse_response(&response_json)
            }
            Err(ureq::Error::StatusCode(429)) => Err(ProviderError::RateLimited {
                retry_after_secs: None,
            }),
            Err(ureq::Error::StatusCode(status)) => {
                let message = format!("HTTP {status}");
                Err(ProviderError::Api { status, message })
            }
            Err(e) => Err(ProviderError::Network(format!("{e:#}"))),
        }
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
// Conversion helpers
// ============================================================================

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
            .filter(|c| match c {
                Content::Text(t) => !t.trim().is_empty(),
                _ => true,
            })
            .map(|c| match c {
                Content::Text(t) => serde_json::json!({"type": "text", "text": t}),
                Content::ToolCall { id, name, input } => serde_json::json!({
                    "type": "tool_use", "id": id, "name": name, "input": input,
                }),
                Content::ToolResult { id, result } => serde_json::json!({
                    "type": "tool_result", "tool_use_id": id, "content": result.to_string(),
                }),
            })
            .collect();

        if content.is_empty() {
            continue;
        }

        if let Some(last) = result.last_mut() {
            let last_obj: &mut serde_json::Value = last;
            if last_obj.get("role").and_then(|r| r.as_str()) == Some(role)
                && let Some(arr) = last_obj.get_mut("content").and_then(|c| c.as_array_mut())
            {
                arr.extend(content);
                continue;
            }
        }

        result.push(serde_json::json!({"role": role, "content": content}));
    }

    result
}

fn convert_tools(tool_definitions: &[serde_json::Value]) -> Vec<serde_json::Value> {
    tool_definitions
        .iter()
        .map(|def| {
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
                .unwrap_or(serde_json::json!({"type": "object", "properties": {}}));

            serde_json::json!({
                "name": name,
                "description": description,
                "input_schema": input_schema,
            })
        })
        .collect()
}

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
            _ => {}
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
            cache_creation_input_tokens: u
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize,
            cache_read_input_tokens: u
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize,
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
