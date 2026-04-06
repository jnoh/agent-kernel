//! Provider implementations for the code-agent distribution.

use kernel_interfaces::provider::{
    ProviderCaps, ProviderError, ProviderInterface, Response, StopReason, Usage,
};
use kernel_interfaces::types::{CompletionConfig, Content, Prompt};

/// A simple echo provider that reads the last user message and responds with it.
/// Useful for testing the full stack without an API key.
pub struct EchoProvider;

impl ProviderInterface for EchoProvider {
    fn complete(&self, prompt: &Prompt, _config: &CompletionConfig) -> Result<Response, ProviderError> {
        // Find the last user message
        let last_user = prompt
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, kernel_interfaces::types::Role::User))
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
                 (This is a stub provider. To use a real model, implement ProviderInterface \
                 for your preferred API and pass it to the session.)"
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
