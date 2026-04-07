//! In-process kernel — same channel-based protocol as the daemon, but
//! running entirely in-process without Unix sockets.
//!
//! This is the backward-compatibility layer for library consumers who want
//! the kernel's protocol semantics without IPC overhead. It's also useful
//! for testing.

use crate::event_loop::{EventLoop, EventLoopConfig};
use crate::proxy_frontend::ProxyFrontend;
use crate::proxy_tool::{ProxyTool, ToolResponse};
use crossbeam_channel::{Receiver, Sender};
use kernel_interfaces::protocol::{KernelEvent, KernelRequest, SessionCreateConfig, ToolSchema};
use kernel_interfaces::provider::ProviderInterface;
use kernel_interfaces::tool::ToolRegistration;
use kernel_interfaces::types::SessionId;
use std::collections::HashMap;
use std::time::Duration;

/// Handle for a session created by InProcessKernel.
pub struct SessionHandle {
    /// Send commands to the session's EventLoop.
    pub request_tx: Sender<KernelRequest>,
    /// Receive events from the session.
    pub event_rx: Receiver<KernelEvent>,
    /// Send tool responses back to ProxyTools.
    pub tool_response_txs: HashMap<String, Sender<ToolResponse>>,
    /// Send permission responses back to ProxyFrontend.
    pub permission_tx: Sender<crate::proxy_frontend::PermissionResponse>,
}

/// An in-process kernel that provides the same protocol interface as the
/// daemon but runs everything in the same process.
pub struct InProcessKernel {
    tool_schemas: Vec<ToolSchema>,
    next_session_id: u64,
}

impl InProcessKernel {
    pub fn new() -> Self {
        Self {
            tool_schemas: Vec::new(),
            next_session_id: 0,
        }
    }

    /// Register tool schemas (same as KernelRequest::RegisterTools).
    pub fn register_tools(&mut self, schemas: Vec<ToolSchema>) {
        self.tool_schemas = schemas;
    }

    /// Create a session and return a handle for sending/receiving protocol messages.
    /// The EventLoop runs on a new thread.
    pub fn create_session(
        &mut self,
        config: SessionCreateConfig,
        provider: Box<dyn ProviderInterface + Send>,
    ) -> SessionHandle {
        let session_id = SessionId(self.next_session_id);
        self.next_session_id += 1;

        let (input_tx, input_rx) = crossbeam_channel::unbounded();
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (permission_tx, permission_rx) = crossbeam_channel::unbounded();

        // Create ProxyTools from schemas
        let mut tools: Vec<Box<dyn ToolRegistration>> = Vec::new();
        let mut tool_response_txs = HashMap::new();

        for schema in &self.tool_schemas {
            let (response_tx, response_rx) = crossbeam_channel::unbounded();
            tool_response_txs.insert(schema.name.clone(), response_tx);

            let proxy = ProxyTool::new(
                schema.clone(),
                session_id,
                event_tx.clone(),
                response_rx,
                Duration::from_secs(120),
            );
            tools.push(Box::new(proxy));
        }

        let frontend = ProxyFrontend::new(
            session_id,
            event_tx.clone(),
            permission_rx,
            Duration::from_secs(300),
        );

        let el_config = EventLoopConfig {
            session_id,
            session_create: config,
            tools,
            provider,
            frontend,
        };

        let mut event_loop = EventLoop::new(el_config, input_rx, event_tx);
        std::thread::spawn(move || {
            event_loop.run();
        });

        SessionHandle {
            request_tx: input_tx,
            event_rx,
            tool_response_txs,
            permission_tx,
        }
    }
}

impl Default for InProcessKernel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use kernel_interfaces::types::*;

    #[test]
    fn in_process_text_only_turn() {
        let mut kernel = InProcessKernel::new();

        let provider = Box::new(FakeProvider {
            response: text_response("Hello from kernel!"),
        });

        let handle = kernel.create_session(
            SessionCreateConfig {
                mode: SessionMode::Interactive,
                system_prompt: "Test.".into(),
                completion_config: CompletionConfig::default(),
                policy: allow_all_policy(),
                resource_budget: ResourceBudget::default(),
                workspace: "/tmp/test".into(),
            },
            provider,
        );

        // Send input
        handle
            .request_tx
            .send(KernelRequest::AddInput {
                session_id: SessionId(0),
                text: "Hi".into(),
            })
            .unwrap();

        // Collect events
        let mut got_text = false;
        loop {
            let event = handle
                .event_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            match event {
                KernelEvent::TextOutput { text, .. } => {
                    assert_eq!(text, "Hello from kernel!");
                    got_text = true;
                }
                KernelEvent::TurnEnded { .. } => break,
                _ => {}
            }
        }

        assert!(got_text);

        handle.request_tx.send(KernelRequest::Shutdown).unwrap();
    }

    #[test]
    fn in_process_tool_execution() {
        let mut kernel = InProcessKernel::new();

        // Register a tool schema
        kernel.register_tools(vec![ToolSchema {
            name: "file_read".into(),
            description: "Read a file".into(),
            capabilities: {
                let mut s = std::collections::HashSet::new();
                s.insert(Capability::new("fs:read"));
                s
            },
            schema: serde_json::json!({"type": "object"}),
            cost: TokenEstimate(100),
            relevance: RelevanceSignal {
                keywords: vec![],
                tags: vec![],
            },
        }]);

        // Provider that requests a tool call
        let provider = Box::new(FakeProvider {
            response: tool_call_response("file_read", serde_json::json!({"path": "main.rs"})),
        });

        let handle = kernel.create_session(
            SessionCreateConfig {
                mode: SessionMode::Interactive,
                system_prompt: "Test.".into(),
                completion_config: CompletionConfig::default(),
                policy: allow_all_policy(),
                resource_budget: ResourceBudget::default(),
                workspace: "/tmp/test".into(),
            },
            provider,
        );

        handle
            .request_tx
            .send(KernelRequest::AddInput {
                session_id: SessionId(0),
                text: "Read main.rs".into(),
            })
            .unwrap();

        // Wait for ExecuteTool event
        loop {
            let event = handle
                .event_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            match event {
                KernelEvent::ExecuteTool {
                    request_id,
                    tool_name,
                    ..
                } => {
                    assert_eq!(tool_name, "file_read");
                    // Send tool result back
                    let tx = handle.tool_response_txs.get("file_read").unwrap();
                    tx.send(ToolResponse {
                        request_id,
                        result: serde_json::json!("fn main() {}"),
                        invalidations: vec![],
                    })
                    .unwrap();
                }
                KernelEvent::TurnEnded { .. } => break,
                _ => {}
            }
        }

        handle.request_tx.send(KernelRequest::Shutdown).unwrap();
    }

    #[test]
    fn in_process_query_session() {
        let mut kernel = InProcessKernel::new();

        let provider = Box::new(FakeProvider {
            response: text_response("Hi"),
        });

        let handle = kernel.create_session(
            SessionCreateConfig {
                mode: SessionMode::Interactive,
                system_prompt: "Test.".into(),
                completion_config: CompletionConfig::default(),
                policy: allow_all_policy(),
                resource_budget: ResourceBudget::default(),
                workspace: "/tmp/test".into(),
            },
            provider,
        );

        handle
            .request_tx
            .send(KernelRequest::QuerySession {
                session_id: SessionId(0),
            })
            .unwrap();

        let event = handle
            .event_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        match event {
            KernelEvent::SessionStatus {
                tokens_used,
                turn_count,
                ..
            } => {
                assert!(tokens_used > 0);
                assert_eq!(turn_count, 0);
            }
            _ => panic!("expected SessionStatus"),
        }

        handle.request_tx.send(KernelRequest::Shutdown).unwrap();
    }
}
