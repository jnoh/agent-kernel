use crate::types::{CapabilitySet, Invalidation, RelevanceSignal, TokenEstimate};
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
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            Self::ExecutionFailed(msg) => write!(f, "execution failed: {msg}"),
            Self::Timeout => write!(f, "tool execution timed out"),
            Self::PermissionDenied(msg) => write!(f, "permission denied: {msg}"),
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
    fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError>;
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
}
