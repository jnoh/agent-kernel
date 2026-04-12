//! Anthropic `ProviderInterface` implementation — real Claude API over
//! HTTPS via `ureq`. Reads `ANTHROPIC_API_KEY` via the caller (the
//! daemon passes it into `new`). Supports prompt caching via the
//! `cache_control` blocks on system / tool / message content when the
//! prefix is above the per-model cache minimum.

use kernel_interfaces::provider::{
    ProviderCaps, ProviderError, ProviderInterface, Response, StopReason, Usage,
};
use kernel_interfaces::types::{CompletionConfig, Content, Message, Prompt, Role, StreamChunk};
use std::io::BufRead;

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

    fn send_request(&self, body_str: &str) -> Result<Response, ProviderError> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS)))
            .build()
            .new_agent();
        let response = agent
            .post("https://api.anthropic.com/v1/messages")
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
            Err(ureq::Error::StatusCode(429)) => {
                // TODO: extract retry-after header when ureq exposes it
                Err(ProviderError::RateLimited {
                    retry_after_secs: None,
                })
            }
            Err(ureq::Error::StatusCode(status)) => {
                let message = format!("HTTP {status}");
                Err(ProviderError::Api { status, message })
            }
            Err(e) => Err(ProviderError::Network(format!("{e:#}"))),
        }
    }

    fn send_streaming_request(
        &self,
        body_str: &str,
        on_chunk: &dyn Fn(StreamChunk),
    ) -> Result<Response, ProviderError> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS)))
            .build()
            .new_agent();
        let response = agent
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .send(body_str.as_bytes());

        match response {
            Ok(resp) => {
                let reader = std::io::BufReader::new(resp.into_body().into_reader());
                parse_sse_stream(reader, on_chunk)
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
}

/// Parse an Anthropic SSE stream into StreamChunks + a final Response.
fn parse_sse_stream<R: BufRead>(
    reader: R,
    on_chunk: &dyn Fn(StreamChunk),
) -> Result<Response, ProviderError> {
    let mut content: Vec<Content> = Vec::new();
    let mut usage = Usage::default();
    let mut stop_reason = StopReason::EndTurn;

    // Per-block accumulators
    let mut current_text = String::new();
    let mut current_tool_id = String::new();
    let mut current_tool_name = String::new();
    let mut current_tool_json = String::new();

    for line in reader.lines() {
        let line = line.map_err(|e| ProviderError::Network(format!("SSE read: {e}")))?;
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        if data.trim().is_empty() {
            continue;
        }
        let event: serde_json::Value = serde_json::from_str(data)
            .map_err(|e| ProviderError::Parse(format!("SSE JSON: {e}")))?;

        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match event_type {
            "content_block_start" => {
                let block = event
                    .get("content_block")
                    .unwrap_or(&serde_json::Value::Null);
                let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if block_type == "tool_use" {
                    // Flush any accumulated text
                    if !current_text.is_empty() {
                        content.push(Content::Text(std::mem::take(&mut current_text)));
                    }
                    current_tool_id = block
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    current_tool_name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    current_tool_json.clear();
                    on_chunk(StreamChunk::ToolCallStart {
                        id: current_tool_id.clone(),
                        name: current_tool_name.clone(),
                    });
                }
            }
            "content_block_delta" => {
                let delta = event.get("delta").unwrap_or(&serde_json::Value::Null);
                let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match delta_type {
                    "text_delta" => {
                        let text = delta
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !text.is_empty() {
                            current_text.push_str(&text);
                            on_chunk(StreamChunk::Text(text));
                        }
                    }
                    "input_json_delta" => {
                        let partial = delta
                            .get("partial_json")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        current_tool_json.push_str(partial);
                        on_chunk(StreamChunk::ToolCallDelta {
                            id: current_tool_id.clone(),
                            input_json: partial.to_string(),
                        });
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                if !current_tool_id.is_empty() {
                    let input: serde_json::Value =
                        serde_json::from_str(&current_tool_json).unwrap_or_default();
                    content.push(Content::ToolCall {
                        id: std::mem::take(&mut current_tool_id),
                        name: std::mem::take(&mut current_tool_name),
                        input,
                    });
                    current_tool_json.clear();
                    on_chunk(StreamChunk::ToolCallEnd {
                        id: content
                            .last()
                            .and_then(|c| match c {
                                Content::ToolCall { id, .. } => Some(id.clone()),
                                _ => None,
                            })
                            .unwrap_or_default(),
                    });
                }
            }
            "message_delta" => {
                let delta = event.get("delta").unwrap_or(&serde_json::Value::Null);
                stop_reason = match delta.get("stop_reason").and_then(|v| v.as_str()) {
                    Some("tool_use") => StopReason::ToolUse,
                    Some("max_tokens") => StopReason::MaxTokens,
                    Some("stop_sequence") => StopReason::StopSequence,
                    _ => StopReason::EndTurn,
                };
                if let Some(u) = event.get("usage") {
                    usage.output_tokens =
                        u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                }
            }
            "message_start" => {
                if let Some(msg) = event.get("message")
                    && let Some(u) = msg.get("usage")
                {
                    usage.input_tokens =
                        u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    usage.cache_creation_input_tokens =
                        u.get("cache_creation_input_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as usize;
                    usage.cache_read_input_tokens = u
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;
                }
            }
            "message_stop" => {
                break;
            }
            _ => {}
        }
    }

    // Flush remaining text
    if !current_text.is_empty() {
        content.push(Content::Text(current_text));
    }

    on_chunk(StreamChunk::Done);

    Ok(Response {
        content,
        usage,
        stop_reason,
    })
}

/// Whether a `ProviderError` is transient and worth retrying.
fn is_transient(err: &ProviderError) -> bool {
    match err {
        ProviderError::RateLimited { .. } => true,
        ProviderError::Network(_) => true,
        ProviderError::Api { status, .. } => (500..600).contains(status),
        ProviderError::Parse(_) => false,
    }
}

/// Max retry attempts for transient errors.
const MAX_RETRIES: u32 = 3;

/// HTTP request timeout in seconds.
const REQUEST_TIMEOUT_SECS: u64 = 120;

impl ProviderInterface for AnthropicProvider {
    fn complete(
        &self,
        prompt: &Prompt,
        config: &CompletionConfig,
    ) -> Result<Response, ProviderError> {
        let body = self.build_request_body(prompt, config);
        let body_str = serde_json::to_string(&body)
            .map_err(|e| ProviderError::Parse(format!("failed to serialize request: {e}")))?;

        let mut last_err: Option<ProviderError> = None;

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                let base_delay = match &last_err {
                    Some(ProviderError::RateLimited {
                        retry_after_secs: Some(secs),
                    }) => *secs as f64,
                    _ => (1u64 << (attempt - 1).min(4)) as f64, // 1, 2, 4s
                };
                // Jitter: +/- 25%
                let jitter = 0.75 + (attempt as f64 * 0.1 % 0.5); // deterministic pseudo-jitter
                let delay = std::time::Duration::from_secs_f64(base_delay * jitter);
                std::thread::sleep(delay.min(std::time::Duration::from_secs(30)));
            }

            let result = self.send_request(&body_str);
            match result {
                Ok(resp) => return Ok(resp),
                Err(ref e) if is_transient(e) && attempt < MAX_RETRIES => {
                    eprintln!(
                        "  [provider] transient error (attempt {}/{}): {e}",
                        attempt + 1,
                        MAX_RETRIES + 1
                    );
                    last_err = Some(result.unwrap_err());
                }
                Err(e) => return Err(e),
            }
        }

        Err(last_err.unwrap_or_else(|| ProviderError::Network("retry exhausted".into())))
    }

    fn complete_stream(
        &self,
        prompt: &Prompt,
        config: &CompletionConfig,
        on_chunk: &dyn Fn(StreamChunk),
    ) -> Result<Response, ProviderError> {
        let mut body = self.build_request_body(prompt, config);
        body["stream"] = serde_json::json!(true);
        let body_str = serde_json::to_string(&body)
            .map_err(|e| ProviderError::Parse(format!("failed to serialize request: {e}")))?;

        let mut last_err: Option<ProviderError> = None;

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                let base_delay = match &last_err {
                    Some(ProviderError::RateLimited {
                        retry_after_secs: Some(secs),
                    }) => *secs as f64,
                    _ => (1u64 << (attempt - 1).min(4)) as f64,
                };
                let jitter = 0.75 + (attempt as f64 * 0.1 % 0.5);
                let delay = std::time::Duration::from_secs_f64(base_delay * jitter);
                std::thread::sleep(delay.min(std::time::Duration::from_secs(30)));
            }

            let result = self.send_streaming_request(&body_str, on_chunk);
            match result {
                Ok(resp) => return Ok(resp),
                Err(ref e) if is_transient(e) && attempt < MAX_RETRIES => {
                    eprintln!(
                        "  [provider] transient error (attempt {}/{}): {e}",
                        attempt + 1,
                        MAX_RETRIES + 1
                    );
                    last_err = Some(result.unwrap_err());
                }
                Err(e) => return Err(e),
            }
        }

        Err(last_err.unwrap_or_else(|| ProviderError::Network("retry exhausted".into())))
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
            supports_streaming: true,
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
                Content::ToolResult { id, result } => {
                    // Serialize strings as bare text, not JSON-quoted.
                    // The Anthropic API content field accepts plain strings.
                    let content_str = match result {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    serde_json::json!({
                        "type": "tool_result", "tool_use_id": id, "content": content_str,
                    })
                }
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
