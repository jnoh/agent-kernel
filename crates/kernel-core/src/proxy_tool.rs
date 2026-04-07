//! A ToolRegistration implementation that bridges to an IPC channel.
//!
//! When the kernel daemon needs to execute a tool, it doesn't run it locally —
//! the distro owns the tool implementations. `ProxyTool` sends an `ExecuteTool`
//! event over a channel and blocks until the distro responds with the result.
//!
//! From `TurnLoop`'s perspective, `ProxyTool` looks exactly like a local tool.

use crossbeam_channel::{Receiver, Sender};
use kernel_interfaces::protocol::{KernelEvent, RequestId, ToolSchema};
use kernel_interfaces::tool::{ToolError, ToolOutput, ToolRegistration};
use kernel_interfaces::types::{CapabilitySet, Invalidation, RelevanceSignal, TokenEstimate};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Shared request ID counter across all ProxyTools in a session.
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(0);

fn next_request_id() -> RequestId {
    RequestId(NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed))
}

/// Response from the distro for a tool execution request.
pub struct ToolResponse {
    pub request_id: RequestId,
    pub result: serde_json::Value,
    pub invalidations: Vec<Invalidation>,
}

/// A tool that proxies execution over IPC channels.
pub struct ProxyTool {
    schema: ToolSchema,
    /// Channel to send KernelEvents to the distro.
    event_tx: Sender<KernelEvent>,
    /// Channel to receive tool execution results from the distro.
    response_rx: Receiver<ToolResponse>,
    /// Session ID for the ExecuteTool event.
    session_id: kernel_interfaces::types::SessionId,
    /// Timeout for waiting on the distro's response.
    timeout: Duration,
}

impl ProxyTool {
    pub fn new(
        schema: ToolSchema,
        session_id: kernel_interfaces::types::SessionId,
        event_tx: Sender<KernelEvent>,
        response_rx: Receiver<ToolResponse>,
        timeout: Duration,
    ) -> Self {
        Self {
            schema,
            event_tx,
            response_rx,
            session_id,
            timeout,
        }
    }
}

impl ToolRegistration for ProxyTool {
    fn name(&self) -> &str {
        &self.schema.name
    }

    fn description(&self) -> &str {
        &self.schema.description
    }

    fn capabilities(&self) -> &CapabilitySet {
        &self.schema.capabilities
    }

    fn schema(&self) -> &serde_json::Value {
        &self.schema.schema
    }

    fn cost(&self) -> TokenEstimate {
        self.schema.cost
    }

    fn relevance(&self) -> &RelevanceSignal {
        &self.schema.relevance
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let request_id = next_request_id();

        // Send the execution request to the distro
        let event = KernelEvent::ExecuteTool {
            session_id: self.session_id,
            request_id,
            tool_name: self.schema.name.clone(),
            input,
        };

        self.event_tx
            .send(event)
            .map_err(|e| ToolError::ExecutionFailed(format!("channel send failed: {e}")))?;

        // Block waiting for the distro's response
        match self.response_rx.recv_timeout(self.timeout) {
            Ok(response) => {
                let output = if response.invalidations.is_empty() {
                    ToolOutput::readonly(response.result)
                } else {
                    ToolOutput::with_invalidations(response.result, response.invalidations)
                };
                Ok(output)
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Err(ToolError::Timeout),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                Err(ToolError::ExecutionFailed("distro disconnected".into()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_interfaces::types::SessionId;

    fn test_schema() -> ToolSchema {
        ToolSchema {
            name: "file_read".into(),
            description: "Read a file".into(),
            capabilities: CapabilitySet::new(),
            schema: serde_json::json!({"type": "object"}),
            cost: TokenEstimate(100),
            relevance: RelevanceSignal {
                keywords: vec![],
                tags: vec![],
            },
        }
    }

    #[test]
    fn proxy_tool_metadata_from_schema() {
        let (event_tx, _event_rx) = crossbeam_channel::unbounded();
        let (_response_tx, response_rx) = crossbeam_channel::unbounded();
        let tool = ProxyTool::new(
            test_schema(),
            SessionId(0),
            event_tx,
            response_rx,
            Duration::from_secs(10),
        );

        assert_eq!(tool.name(), "file_read");
        assert_eq!(tool.description(), "Read a file");
        assert_eq!(tool.cost(), TokenEstimate(100));
    }

    #[test]
    fn proxy_tool_execute_round_trip() {
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (response_tx, response_rx) = crossbeam_channel::unbounded();
        let tool = ProxyTool::new(
            test_schema(),
            SessionId(0),
            event_tx,
            response_rx,
            Duration::from_secs(10),
        );

        // Simulate the distro in another thread
        let handle = std::thread::spawn(move || {
            let event = event_rx.recv().unwrap();
            match event {
                KernelEvent::ExecuteTool {
                    request_id, input, ..
                } => {
                    assert_eq!(input, serde_json::json!({"path": "main.rs"}));
                    response_tx
                        .send(ToolResponse {
                            request_id,
                            result: serde_json::json!("fn main() {}"),
                            invalidations: vec![],
                        })
                        .unwrap();
                }
                _ => panic!("unexpected event"),
            }
        });

        let output = tool
            .execute(serde_json::json!({"path": "main.rs"}))
            .unwrap();
        assert_eq!(output.result, serde_json::json!("fn main() {}"));
        assert!(output.invalidations.is_empty());

        handle.join().unwrap();
    }

    #[test]
    fn proxy_tool_timeout() {
        let (event_tx, _event_rx) = crossbeam_channel::unbounded();
        let (_response_tx, response_rx) = crossbeam_channel::unbounded();
        let tool = ProxyTool::new(
            test_schema(),
            SessionId(0),
            event_tx,
            response_rx,
            Duration::from_millis(10), // very short timeout
        );

        let result = tool.execute(serde_json::json!({}));
        assert!(matches!(result, Err(ToolError::Timeout)));
    }

    #[test]
    fn proxy_tool_disconnected() {
        let (event_tx, _event_rx) = crossbeam_channel::unbounded();
        let (response_tx, response_rx) = crossbeam_channel::unbounded();
        // Drop the sender to simulate disconnection
        drop(response_tx);

        let tool = ProxyTool::new(
            test_schema(),
            SessionId(0),
            event_tx,
            response_rx,
            Duration::from_secs(10),
        );

        let result = tool.execute(serde_json::json!({}));
        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
    }
}
