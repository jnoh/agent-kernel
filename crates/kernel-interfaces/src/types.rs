use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

/// Unique identifier for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub u64);

/// Unique identifier for a turn within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnId(pub u64);

/// A capability declaration — what system resources a tool touches.
/// Examples: "fs:read", "fs:write", "net:api.github.com", "shell:exec", "env:read"
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Capability(pub String);

impl Capability {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Check if this capability matches a pattern.
    /// "net:*" matches "net:api.github.com"
    /// "fs:read" matches only "fs:read"
    pub fn matches(&self, pattern: &Capability) -> bool {
        if pattern.0.ends_with(":*") {
            let prefix = &pattern.0[..pattern.0.len() - 1];
            self.0.starts_with(prefix)
        } else {
            self.0 == pattern.0
        }
    }
}

/// Set of capabilities a tool declares.
pub type CapabilitySet = HashSet<Capability>;

/// Token count estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenEstimate(pub usize);

/// Signals that tell the context manager when to demand-page a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelevanceSignal {
    /// Keyword triggers — if the model's output or user input contains these,
    /// this tool becomes a candidate for demand-paging.
    pub keywords: Vec<String>,

    /// Free-form tags for categorization.
    pub tags: Vec<String>,
}

/// What a tool's execution invalidated in the kernel's cached state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Invalidation {
    /// These cached file contents are stale — re-read before using.
    Files(Vec<PathBuf>),

    /// The workspace root has changed — all relative paths are stale.
    WorkingDirectory(PathBuf),

    /// The set of available tools has changed — re-scan the registry.
    ToolRegistry,

    /// These environment variables changed.
    Environment(Vec<String>),
}

/// The result of a permission evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(String),
    Ask,
}

/// How a session operates — interactive (human attached) or autonomous (policy-driven).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionMode {
    Interactive,
    Autonomous,
}

/// Content that can appear in a prompt message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Content {
    Text(String),
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        id: String,
        result: serde_json::Value,
    },
}

/// A message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Content>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
}

/// A fully assembled prompt ready to send to a provider.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub system: String,
    pub messages: Vec<Message>,
    pub tool_definitions: Vec<serde_json::Value>,
}

/// Configuration for a completion request.
#[derive(Debug, Clone)]
pub struct CompletionConfig {
    pub max_tokens: usize,
    pub temperature: Option<f64>,
    pub stop_sequences: Vec<String>,
}

impl Default for CompletionConfig {
    fn default() -> Self {
        Self {
            max_tokens: 4096,
            temperature: None,
            stop_sequences: Vec::new(),
        }
    }
}

/// A streaming chunk from the model.
#[derive(Debug, Clone)]
pub enum StreamChunk {
    Text(String),
    ToolCallStart { id: String, name: String },
    ToolCallDelta { id: String, input_json: String },
    ToolCallEnd { id: String },
    Done,
}

/// Resource budget limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceBudget {
    pub max_tokens_per_session: usize,
    pub max_tool_invocations_per_turn: usize,
    pub max_wall_time_per_tool_secs: u64,
    pub max_output_size_bytes: usize,
}

impl Default for ResourceBudget {
    fn default() -> Self {
        Self {
            max_tokens_per_session: 1_000_000,
            max_tool_invocations_per_turn: 20,
            max_wall_time_per_tool_secs: 120,
            max_output_size_bytes: 100 * 1024,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_exact_match() {
        let cap = Capability::new("fs:read");
        let pattern = Capability::new("fs:read");
        assert!(cap.matches(&pattern));
    }

    #[test]
    fn capability_wildcard_match() {
        let cap = Capability::new("net:api.github.com");
        let wildcard = Capability::new("net:*");
        assert!(cap.matches(&wildcard));
    }

    #[test]
    fn capability_no_match() {
        let cap = Capability::new("fs:read");
        let pattern = Capability::new("fs:write");
        assert!(!cap.matches(&pattern));
    }

    #[test]
    fn capability_wildcard_does_not_cross_categories() {
        let cap = Capability::new("shell:exec");
        let wildcard = Capability::new("net:*");
        assert!(!cap.matches(&wildcard));
    }
}
