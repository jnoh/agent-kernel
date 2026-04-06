use crate::tool::ToolOutput;
use crate::types::{Decision, StreamChunk, TurnId};
use std::path::Path;

/// Summary of a compaction event, for frontend display.
#[derive(Debug, Clone)]
pub struct CompactionSummary {
    pub turns_before: usize,
    pub turns_after: usize,
    pub tokens_freed: usize,
}

/// Permission request that the frontend must resolve.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub tool_name: String,
    pub capabilities: Vec<String>,
    pub input_summary: String,
}

/// Errors from kernel operations surfaced to the frontend.
#[derive(Debug, Clone)]
pub struct KernelError {
    pub message: String,
    pub recoverable: bool,
}

/// Event notifications from the kernel to the frontend.
/// The core never knows whether it's talking to a TUI, IDE extension, or web dashboard.
pub trait FrontendEvents: Send {
    /// A new turn is starting.
    fn on_turn_start(&self, turn_id: TurnId);

    /// Streaming chunk from the model.
    fn on_stream_chunk(&self, chunk: &StreamChunk);

    /// The model produced text output (non-streaming path).
    fn on_text(&self, text: &str);

    /// A tool is being called.
    fn on_tool_call(&self, tool_name: &str, input: &serde_json::Value);

    /// A tool produced a result.
    fn on_tool_result(&self, tool_name: &str, result: &ToolOutput);

    /// Permission required — returns user's decision.
    fn on_permission_request(&self, request: &PermissionRequest) -> Decision;

    /// The turn is complete.
    fn on_turn_end(&self, turn_id: TurnId);

    /// Context was compacted.
    fn on_compaction(&self, summary: &CompactionSummary);

    /// The workspace root changed.
    fn on_workspace_changed(&self, new_root: &Path);

    /// Error occurred.
    fn on_error(&self, error: &KernelError);
}

/// The command surface for frontends to control a running session.
/// Complements FrontendEvents: events flow out, commands flow in.
pub trait SessionControl: Send {
    // --- Queries ---
    fn tokens_used(&self) -> usize;
    fn context_utilization(&self) -> f64;
    fn turn_count(&self) -> usize;

    // --- Commands ---
    /// Signal cancellation — the turn loop should stop dispatching tools.
    fn cancel(&self);
    /// Force context compaction. Returns tokens freed.
    fn request_compaction(&mut self) -> Result<usize, String>;
    /// Hot-swap the active policy.
    fn set_policy(&mut self, policy: crate::policy::Policy);
}
