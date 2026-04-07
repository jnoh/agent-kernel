//! Protocol message types for kernel ↔ distro IPC.
//!
//! The kernel daemon and distro processes communicate via length-prefixed JSON
//! messages over a Unix domain socket. This module defines all message types.

use crate::channel::ExternalEvent;
use crate::frontend::{CompactionSummary, KernelError, PermissionRequest};
use crate::policy::Policy;
use crate::types::{
    CapabilitySet, CompletionConfig, Decision, Invalidation, RelevanceSignal, ResourceBudget,
    SessionId, SessionMode, TokenEstimate, TurnId,
};
use serde::{Deserialize, Serialize};

/// Correlates async request/response pairs (tool execution, permission).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(pub u64);

/// The serializable subset of ToolRegistration — everything except execute().
/// Sent by the distro during tool registration; used by the kernel to build
/// ProxyTool instances.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub capabilities: CapabilitySet,
    pub schema: serde_json::Value,
    pub cost: TokenEstimate,
    pub relevance: RelevanceSignal,
}

/// Configuration for creating a new session, sent by the distro.
/// The distro does not configure the provider — the kernel owns that.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCreateConfig {
    pub mode: SessionMode,
    pub system_prompt: String,
    pub completion_config: CompletionConfig,
    pub policy: Policy,
    pub resource_budget: ResourceBudget,
    pub workspace: String,
}

/// Summary of a completed turn, sent from kernel to distro.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnResultSummary {
    pub tool_calls_dispatched: usize,
    pub tool_calls_denied: usize,
    pub was_cancelled: bool,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub cache_creation_input_tokens: usize,
    pub cache_read_input_tokens: usize,
}

// ---------------------------------------------------------------------------
// Messages: Distro → Kernel
// ---------------------------------------------------------------------------

/// Messages sent from the distro to the kernel daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KernelRequest {
    /// Register tools that the distro can execute.
    /// Must be sent before CreateSession.
    RegisterTools { tools: Vec<ToolSchema> },

    /// Create a new session with the given configuration.
    CreateSession { config: SessionCreateConfig },

    /// Feed user or event-source input into a session.
    AddInput { session_id: SessionId, text: String },

    /// Deliver an external event to a session.
    DeliverEvent {
        session_id: SessionId,
        event: ExternalEvent,
    },

    /// Return the result of a tool execution requested by the kernel.
    ToolResult {
        request_id: RequestId,
        result: serde_json::Value,
        invalidations: Vec<Invalidation>,
    },

    /// Respond to a permission request from the kernel.
    PermissionResponse {
        request_id: RequestId,
        decision: Decision,
    },

    /// Cancel the current turn in a session.
    CancelTurn { session_id: SessionId },

    /// Request context compaction for a session.
    RequestCompaction { session_id: SessionId },

    /// Hot-swap the policy for a session.
    SetPolicy {
        session_id: SessionId,
        policy: Policy,
    },

    /// Query session status (tokens, utilization, turn count).
    QuerySession { session_id: SessionId },

    /// Shut down the kernel daemon.
    Shutdown,
}

// ---------------------------------------------------------------------------
// Messages: Kernel → Distro
// ---------------------------------------------------------------------------

/// Messages sent from the kernel daemon to the distro.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KernelEvent {
    /// A new session was created.
    SessionCreated { session_id: SessionId },

    /// The kernel requests the distro to execute a tool.
    /// The distro must respond with a ToolResult KernelRequest.
    ExecuteTool {
        session_id: SessionId,
        request_id: RequestId,
        tool_name: String,
        input: serde_json::Value,
    },

    /// The kernel needs a permission decision from the user.
    /// The distro must respond with a PermissionResponse KernelRequest.
    PermissionRequired {
        session_id: SessionId,
        request_id: RequestId,
        request: PermissionRequest,
    },

    /// The model produced text output.
    TextOutput { session_id: SessionId, text: String },

    /// A tool was called (informational, before execution).
    ToolCallStarted {
        session_id: SessionId,
        tool_name: String,
        input: serde_json::Value,
    },

    /// A turn started.
    TurnStarted {
        session_id: SessionId,
        turn_id: TurnId,
    },

    /// A turn ended.
    TurnEnded {
        session_id: SessionId,
        turn_id: TurnId,
        result: TurnResultSummary,
    },

    /// Context was compacted.
    CompactionHappened {
        session_id: SessionId,
        summary: CompactionSummary,
    },

    /// Response to a QuerySession request.
    SessionStatus {
        session_id: SessionId,
        tokens_used: usize,
        utilization: f64,
        turn_count: usize,
    },

    /// An error occurred.
    Error {
        session_id: Option<SessionId>,
        error: KernelError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_request_round_trip() {
        let requests = vec![
            KernelRequest::RegisterTools {
                tools: vec![ToolSchema {
                    name: "file_read".into(),
                    description: "Read a file".into(),
                    capabilities: CapabilitySet::new(),
                    schema: serde_json::json!({"type": "object"}),
                    cost: TokenEstimate(100),
                    relevance: RelevanceSignal {
                        keywords: vec!["read".into()],
                        tags: vec!["fs".into()],
                    },
                }],
            },
            KernelRequest::CreateSession {
                config: SessionCreateConfig {
                    mode: SessionMode::Interactive,
                    system_prompt: "You are helpful.".into(),
                    completion_config: CompletionConfig::default(),
                    policy: Policy {
                        version: 1,
                        name: "test".into(),
                        rules: Vec::new(),
                        resource_budgets: None,
                    },
                    resource_budget: ResourceBudget::default(),
                    workspace: "/tmp/test".into(),
                },
            },
            KernelRequest::AddInput {
                session_id: SessionId(0),
                text: "Hello".into(),
            },
            KernelRequest::ToolResult {
                request_id: RequestId(1),
                result: serde_json::json!("file contents"),
                invalidations: vec![],
            },
            KernelRequest::PermissionResponse {
                request_id: RequestId(2),
                decision: Decision::Allow,
            },
            KernelRequest::CancelTurn {
                session_id: SessionId(0),
            },
            KernelRequest::RequestCompaction {
                session_id: SessionId(0),
            },
            KernelRequest::QuerySession {
                session_id: SessionId(0),
            },
            KernelRequest::Shutdown,
        ];

        for req in &requests {
            let json = serde_json::to_string(req).expect("serialize");
            let round_tripped: KernelRequest = serde_json::from_str(&json).expect("deserialize");
            let json2 = serde_json::to_string(&round_tripped).expect("re-serialize");
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn kernel_event_round_trip() {
        let events = vec![
            KernelEvent::SessionCreated {
                session_id: SessionId(0),
            },
            KernelEvent::ExecuteTool {
                session_id: SessionId(0),
                request_id: RequestId(1),
                tool_name: "file_read".into(),
                input: serde_json::json!({"path": "src/main.rs"}),
            },
            KernelEvent::PermissionRequired {
                session_id: SessionId(0),
                request_id: RequestId(2),
                request: PermissionRequest {
                    tool_name: "shell".into(),
                    capabilities: vec!["shell:exec".into()],
                    input_summary: "rm -rf /".into(),
                },
            },
            KernelEvent::TextOutput {
                session_id: SessionId(0),
                text: "Hello!".into(),
            },
            KernelEvent::ToolCallStarted {
                session_id: SessionId(0),
                tool_name: "file_read".into(),
                input: serde_json::json!({}),
            },
            KernelEvent::TurnStarted {
                session_id: SessionId(0),
                turn_id: TurnId(0),
            },
            KernelEvent::TurnEnded {
                session_id: SessionId(0),
                turn_id: TurnId(0),
                result: TurnResultSummary {
                    tool_calls_dispatched: 1,
                    tool_calls_denied: 0,
                    was_cancelled: false,
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
            },
            KernelEvent::CompactionHappened {
                session_id: SessionId(0),
                summary: CompactionSummary {
                    turns_before: 10,
                    turns_after: 5,
                    tokens_freed: 5000,
                },
            },
            KernelEvent::SessionStatus {
                session_id: SessionId(0),
                tokens_used: 1000,
                utilization: 0.5,
                turn_count: 3,
            },
            KernelEvent::Error {
                session_id: Some(SessionId(0)),
                error: KernelError {
                    message: "something broke".into(),
                    recoverable: true,
                },
            },
            KernelEvent::Error {
                session_id: None,
                error: KernelError {
                    message: "fatal".into(),
                    recoverable: false,
                },
            },
        ];

        for event in &events {
            let json = serde_json::to_string(event).expect("serialize");
            let round_tripped: KernelEvent = serde_json::from_str(&json).expect("deserialize");
            let json2 = serde_json::to_string(&round_tripped).expect("re-serialize");
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn tool_schema_round_trip() {
        let schema = ToolSchema {
            name: "grep".into(),
            description: "Search files".into(),
            capabilities: {
                let mut set = CapabilitySet::new();
                set.insert(crate::types::Capability::new("fs:read"));
                set
            },
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string"}
                },
                "required": ["pattern"]
            }),
            cost: TokenEstimate(150),
            relevance: RelevanceSignal {
                keywords: vec!["search".into(), "find".into(), "grep".into()],
                tags: vec!["fs".into(), "search".into()],
            },
        };

        let json = serde_json::to_string(&schema).expect("serialize");
        let round_tripped: ToolSchema = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(schema.name, round_tripped.name);
        assert_eq!(schema.cost, round_tripped.cost);
    }
}
