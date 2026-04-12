use crate::types::{CapabilitySet, Invalidation, RelevanceSignal, TokenEstimate};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Errors from tool execution.
#[derive(Debug)]
pub enum ToolError {
    /// The input was invalid (bad schema, missing fields).
    InvalidInput(String),
    /// The tool execution failed.
    ExecutionFailed(String),
    /// The tool timed out.
    Timeout,
    /// Permission was denied by the dispatch gate.
    PermissionDenied(String),
    /// Transport-level failure (subprocess died, pipe broken, etc.).
    /// Used by out-of-process toolset transports. In-process tools
    /// should prefer `ExecutionFailed`.
    Transport(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            Self::ExecutionFailed(msg) => write!(f, "execution failed: {msg}"),
            Self::Timeout => write!(f, "tool execution timed out"),
            Self::PermissionDenied(msg) => write!(f, "permission denied: {msg}"),
            Self::Transport(msg) => write!(f, "tool transport error: {msg}"),
        }
    }
}

impl std::error::Error for ToolError {}

/// The result of a tool execution, including invalidation signals.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// The result the model sees.
    pub result: serde_json::Value,

    /// What this tool's execution invalidated.
    /// Empty for read-only tools.
    pub invalidations: Vec<Invalidation>,
}

impl ToolOutput {
    /// Create an output with no invalidations (read-only tool).
    pub fn readonly(result: serde_json::Value) -> Self {
        Self {
            result,
            invalidations: Vec::new(),
        }
    }

    /// Create an output with invalidations (write tool).
    pub fn with_invalidations(result: serde_json::Value, invalidations: Vec<Invalidation>) -> Self {
        Self {
            result,
            invalidations,
        }
    }

    /// Create a denied result to feed back to the model.
    pub fn denied(reason: &str) -> Self {
        Self {
            result: serde_json::json!({ "error": "permission_denied", "reason": reason }),
            invalidations: Vec::new(),
        }
    }
}

/// Which stream an incremental tool output chunk belongs to.
///
/// Shell-like tools distinguish stdout from stderr; text-producing tools
/// that don't have that distinction use `Text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChunkStream {
    Stdout,
    Stderr,
    Text,
}

/// An incremental chunk of tool output emitted while a tool is still running.
///
/// Spec 0015 wires the plumbing (`ToolExecutionCtx` → `FrontendEvents`) but
/// no first-party tool actually emits chunks yet. Spec 0016 adds
/// stdout/stderr streaming to the shell tool when it moves out-of-process.
#[derive(Debug, Clone)]
pub struct ToolChunk {
    pub stream: ToolChunkStream,
    pub data: String,
}

/// Context passed into `ToolRegistration::execute` for each call.
///
/// Currently carries just a chunk sink — a callback the tool can use to
/// emit incremental output while a long-running call is still producing
/// its final result. Non-streaming tools ignore the ctx entirely.
///
/// The sink is `!Send + !Sync` intentionally: `execute` runs synchronously
/// on a single thread, so the ctx lives on that thread and forwards to a
/// local frontend reference. If a future toolset transport needs to push
/// chunks from a background thread, it should use an internal channel and
/// call `emit_chunk` from the execute thread only.
pub struct ToolExecutionCtx<'a> {
    chunk_sink: Option<&'a dyn Fn(ToolChunk)>,
}

impl<'a> ToolExecutionCtx<'a> {
    /// A ctx with no sink — chunks go nowhere. Use this in tests and
    /// places that don't care about streaming.
    pub const fn null() -> ToolExecutionCtx<'static> {
        ToolExecutionCtx { chunk_sink: None }
    }

    /// A ctx whose `emit_chunk` calls the provided closure.
    pub fn with_sink(sink: &'a dyn Fn(ToolChunk)) -> Self {
        Self {
            chunk_sink: Some(sink),
        }
    }

    /// Emit a chunk to the sink, if any.
    pub fn emit_chunk(&self, chunk: ToolChunk) {
        if let Some(sink) = self.chunk_sink {
            sink(chunk);
        }
    }
}

impl Default for ToolExecutionCtx<'_> {
    fn default() -> Self {
        Self::null()
    }
}

/// The chokepoint contract. Every tool — built-in, MCP bridge, or user-defined —
/// registers against this interface.
pub trait ToolRegistration: Send + Sync {
    /// Human-readable name (used for dispatch and display).
    fn name(&self) -> &str;

    /// Description for the model.
    fn description(&self) -> &str;

    /// What system resources this tool touches.
    fn capabilities(&self) -> &CapabilitySet;

    /// JSON Schema for the model to call this tool.
    fn schema(&self) -> &serde_json::Value;

    /// Approximate tokens consumed when this tool's schema is in context.
    fn cost(&self) -> TokenEstimate;

    /// When this tool should be demand-paged into context.
    fn relevance(&self) -> &RelevanceSignal;

    /// Execute the tool.
    ///
    /// The `ctx` carries a chunk-emission sink for streaming tools; most
    /// tools ignore it. Callers that don't care about streaming can pass
    /// `&ToolExecutionCtx::null()`.
    fn execute(
        &self,
        input: serde_json::Value,
        ctx: &ToolExecutionCtx<'_>,
    ) -> Result<ToolOutput, ToolError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_output_readonly_has_no_invalidations() {
        let output = ToolOutput::readonly(serde_json::json!("hello"));
        assert!(output.invalidations.is_empty());
    }

    #[test]
    fn tool_output_denied_contains_reason() {
        let output = ToolOutput::denied("not allowed");
        let reason = output.result.get("reason").unwrap().as_str().unwrap();
        assert_eq!(reason, "not allowed");
    }

    #[test]
    fn null_ctx_drops_chunks_silently() {
        let ctx = ToolExecutionCtx::null();
        ctx.emit_chunk(ToolChunk {
            stream: ToolChunkStream::Stdout,
            data: "hi".into(),
        });
    }

    #[test]
    fn with_sink_ctx_forwards_chunks() {
        use std::sync::Mutex;
        let captured: Mutex<Vec<ToolChunk>> = Mutex::new(Vec::new());
        let sink = |c: ToolChunk| captured.lock().unwrap().push(c);
        let ctx = ToolExecutionCtx::with_sink(&sink);
        ctx.emit_chunk(ToolChunk {
            stream: ToolChunkStream::Stdout,
            data: "one".into(),
        });
        ctx.emit_chunk(ToolChunk {
            stream: ToolChunkStream::Stderr,
            data: "two".into(),
        });
        let got = captured.lock().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].data, "one");
        assert!(matches!(got[1].stream, ToolChunkStream::Stderr));
    }
}
