//! Echo provider — a stub `ProviderInterface` impl that repeats the
//! user's last message back. Used by the daemon as a fallback when
//! `ANTHROPIC_API_KEY` isn't set, and by integration tests that don't
//! want to hit a real model.

use kernel_interfaces::provider::{
    ProviderCaps, ProviderError, ProviderInterface, Response, StopReason, Usage,
};
use kernel_interfaces::types::{CompletionConfig, Content, Prompt, Role};

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
