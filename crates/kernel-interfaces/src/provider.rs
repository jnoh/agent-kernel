use crate::types::{CompletionConfig, Content, Prompt, StreamChunk};
use std::fmt;

/// Errors from provider operations.
#[derive(Debug)]
pub enum ProviderError {
    /// The API returned an error.
    Api { status: u16, message: String },
    /// Network or connection failure.
    Network(String),
    /// The response could not be parsed.
    Parse(String),
    /// Rate limited — includes retry-after hint if available.
    RateLimited { retry_after_secs: Option<u64> },
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api { status, message } => write!(f, "API error {status}: {message}"),
            Self::Network(msg) => write!(f, "network error: {msg}"),
            Self::Parse(msg) => write!(f, "parse error: {msg}"),
            Self::RateLimited { retry_after_secs } => {
                write!(f, "rate limited")?;
                if let Some(secs) = retry_after_secs {
                    write!(f, " (retry after {secs}s)")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ProviderError {}

/// What a provider supports.
#[derive(Debug, Clone, Default)]
pub struct ProviderCaps {
    pub supports_tool_use: bool,
    pub supports_vision: bool,
    pub supports_streaming: bool,
    pub max_context_tokens: usize,
}

/// The model's response from a non-streaming completion.
#[derive(Debug, Clone)]
pub struct Response {
    pub content: Vec<Content>,
    pub usage: Usage,
    pub stop_reason: StopReason,
}

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: usize,
    pub output_tokens: usize,
    /// Tokens written to the prompt cache this request.
    pub cache_creation_input_tokens: usize,
    /// Tokens read from the prompt cache this request (90% cheaper).
    pub cache_read_input_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
}

/// The stable provider contract. The turn loop calls these methods —
/// it never touches provider-specific APIs.
pub trait ProviderInterface {
    /// Blocking completion.
    fn complete(
        &self,
        prompt: &Prompt,
        config: &CompletionConfig,
    ) -> Result<Response, ProviderError>;

    /// Streaming completion. Calls `on_chunk` for each incremental piece
    /// of the response, then returns the fully assembled `Response`.
    /// Default implementation falls back to `complete()` and synthesizes
    /// chunks from the assembled response.
    fn complete_stream(
        &self,
        prompt: &Prompt,
        config: &CompletionConfig,
        on_chunk: &dyn Fn(StreamChunk),
    ) -> Result<Response, ProviderError> {
        let response = self.complete(prompt, config)?;
        for c in &response.content {
            match c {
                Content::Text(t) => on_chunk(StreamChunk::Text(t.clone())),
                Content::ToolCall { id, name, input } => {
                    on_chunk(StreamChunk::ToolCallStart {
                        id: id.clone(),
                        name: name.clone(),
                    });
                    on_chunk(StreamChunk::ToolCallDelta {
                        id: id.clone(),
                        input_json: input.to_string(),
                    });
                    on_chunk(StreamChunk::ToolCallEnd { id: id.clone() });
                }
                _ => {}
            }
        }
        on_chunk(StreamChunk::Done);
        Ok(response)
    }

    /// Token counting for budget management.
    fn count_tokens(&self, content: &Content) -> usize;

    /// What this provider supports.
    fn capabilities(&self) -> ProviderCaps;
}
