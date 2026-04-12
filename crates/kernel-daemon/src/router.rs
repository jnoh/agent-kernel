//! Connection router — dispatches protocol messages between the socket and
//! per-session EventLoops.

use crossbeam_channel::Sender;
use kernel_core::event_loop::{EventLoop, EventLoopConfig};
use kernel_core::proxy_frontend::ProxyFrontend;
use kernel_core::proxy_tool::{ProxyTool, ToolResponse};
use kernel_core::session_events::{
    FileSink, HttpSink, NullSink, SessionEventSink, TeeSink, default_events_path,
};
use kernel_interfaces::protocol::{KernelEvent, KernelRequest, ToolSchema};
use kernel_interfaces::provider::ProviderInterface;
use kernel_interfaces::tool::ToolRegistration;
use kernel_interfaces::types::SessionId;
use std::collections::HashMap;
use std::time::Duration;

use kernel_providers::{AnthropicProvider, EchoProvider};

/// Per-session state tracked by the router.
struct SessionEntry {
    input_tx: Sender<KernelRequest>,
    /// Per-tool response channels keyed by tool name.
    /// Each ProxyTool gets its own response channel.
    tool_response_txs: HashMap<String, Sender<ToolResponse>>,
    /// Permission response channel for the ProxyFrontend.
    permission_tx: Sender<kernel_core::proxy_frontend::PermissionResponse>,
}

/// The connection router manages sessions, routes messages, and owns the provider.
pub struct ConnectionRouter {
    tool_schemas: Vec<ToolSchema>,
    sessions: HashMap<SessionId, SessionEntry>,
    next_session_id: u64,
    /// Channel for outgoing events to the socket writer.
    event_tx: Sender<KernelEvent>,
    /// Provider factory config.
    api_key: Option<String>,
    model: String,
}

impl ConnectionRouter {
    pub fn new(event_tx: Sender<KernelEvent>, api_key: Option<String>, model: String) -> Self {
        Self {
            tool_schemas: Vec::new(),
            sessions: HashMap::new(),
            next_session_id: 0,
            event_tx,
            api_key,
            model,
        }
    }

    /// Handle an incoming request from the distro.
    pub fn handle_request(&mut self, request: KernelRequest) -> bool {
        match request {
            KernelRequest::RegisterTools { tools } => {
                eprintln!("  registered {} tools", tools.len());
                self.tool_schemas = tools;
            }

            KernelRequest::CreateSession { config } => {
                let session_id = SessionId(self.next_session_id);
                eprintln!(
                    "  creating session {:?} with {} tools",
                    session_id,
                    self.tool_schemas.len()
                );
                self.next_session_id += 1;

                // Create per-session channels
                let (input_tx, input_rx) = crossbeam_channel::unbounded();
                let (permission_tx, permission_rx) = crossbeam_channel::unbounded();

                // Create ProxyTools from registered schemas
                let mut tools: Vec<Box<dyn ToolRegistration>> = Vec::new();
                let mut tool_response_txs = HashMap::new();

                for schema in &self.tool_schemas {
                    let (response_tx, response_rx) = crossbeam_channel::unbounded();
                    tool_response_txs.insert(schema.name.clone(), response_tx);

                    let proxy = ProxyTool::new(
                        schema.clone(),
                        session_id,
                        self.event_tx.clone(),
                        response_rx,
                        Duration::from_secs(120),
                    );
                    tools.push(Box::new(proxy));
                }

                // Create provider
                let provider: Box<dyn ProviderInterface + Send> =
                    if let Some(ref key) = self.api_key {
                        Box::new(AnthropicProvider::new(key.clone(), self.model.clone()))
                    } else {
                        Box::new(EchoProvider)
                    };

                // Create ProxyFrontend
                let frontend = ProxyFrontend::new(
                    session_id,
                    self.event_tx.clone(),
                    permission_rx,
                    Duration::from_secs(300), // 5 min for permission decisions
                );

                // Construct the session-events sink. Local path:
                // $AGENT_KERNEL_HOME or $HOME/.agent-kernel/sessions/{id}/events.jsonl.
                // If that base dir is unwritable, fall back to a NullSink.
                // If AGENT_KERNEL_REMOTE_SINK_URL is set, tee every event
                // to an HttpSink as well (spec 0007 — audit-only).
                let events: Box<dyn SessionEventSink> = {
                    let log_path = default_events_path(session_id);
                    let local: Box<dyn SessionEventSink> =
                        match FileSink::new(session_id, &log_path) {
                            Ok(sink) => Box::new(sink),
                            Err(e) => {
                                eprintln!(
                                    "  session_events: failed to open {} ({e}); using NullSink",
                                    log_path.display()
                                );
                                Box::new(NullSink::new(session_id))
                            }
                        };

                    match std::env::var("AGENT_KERNEL_REMOTE_SINK_URL") {
                        Ok(url) if !url.is_empty() => {
                            let token = std::env::var("AGENT_KERNEL_REMOTE_SINK_TOKEN").ok();
                            match HttpSink::new(session_id, &url, token) {
                                Ok(remote) => {
                                    eprintln!(
                                        "  session_events: teeing to remote sink {}",
                                        remote.endpoint()
                                    );
                                    Box::new(TeeSink::new(local, remote))
                                }
                                Err(e) => {
                                    eprintln!(
                                        "  session_events: bad remote sink URL ({e}); local only"
                                    );
                                    local
                                }
                            }
                        }
                        _ => local,
                    }
                };

                // Build EventLoop config
                let el_config = EventLoopConfig {
                    session_id,
                    session_create: config,
                    tools,
                    provider,
                    frontend,
                    events,
                };

                // Spawn EventLoop on its own thread
                let mut event_loop = EventLoop::new(el_config, input_rx, self.event_tx.clone());
                std::thread::spawn(move || {
                    event_loop.run();
                });

                // Track the session
                self.sessions.insert(
                    session_id,
                    SessionEntry {
                        input_tx,
                        tool_response_txs,
                        permission_tx,
                    },
                );

                let _ = self
                    .event_tx
                    .send(KernelEvent::SessionCreated { session_id });
            }

            KernelRequest::AddInput { session_id, text } => {
                if let Some(entry) = self.sessions.get(&session_id) {
                    eprintln!("  routing AddInput to session {:?}", session_id);
                    let _ = entry
                        .input_tx
                        .send(KernelRequest::AddInput { session_id, text });
                } else {
                    eprintln!(
                        "  AddInput: session {:?} not found (have: {:?})",
                        session_id,
                        self.sessions.keys().collect::<Vec<_>>()
                    );
                }
            }

            KernelRequest::DeliverEvent { session_id, event } => {
                if let Some(entry) = self.sessions.get(&session_id) {
                    let _ = entry
                        .input_tx
                        .send(KernelRequest::DeliverEvent { session_id, event });
                }
            }

            KernelRequest::ToolResult {
                request_id,
                result,
                invalidations,
            } => {
                // Route the tool result to the correct ProxyTool.
                // We need to find which session/tool this request_id belongs to.
                // For now, broadcast to all sessions — the ProxyTool will match
                // on its own channel. In production, we'd track request_id → session.
                for entry in self.sessions.values() {
                    for tx in entry.tool_response_txs.values() {
                        let _ = tx.send(ToolResponse {
                            request_id,
                            result: result.clone(),
                            invalidations: invalidations.clone(),
                        });
                    }
                }
            }

            KernelRequest::PermissionResponse {
                request_id,
                decision,
            } => {
                // Route to the correct session's ProxyFrontend.
                for entry in self.sessions.values() {
                    let _ =
                        entry
                            .permission_tx
                            .send(kernel_core::proxy_frontend::PermissionResponse {
                                request_id,
                                decision: decision.clone(),
                            });
                }
            }

            KernelRequest::CancelTurn { session_id } => {
                if let Some(entry) = self.sessions.get(&session_id) {
                    let _ = entry
                        .input_tx
                        .send(KernelRequest::CancelTurn { session_id });
                }
            }

            KernelRequest::RequestCompaction { session_id } => {
                if let Some(entry) = self.sessions.get(&session_id) {
                    let _ = entry
                        .input_tx
                        .send(KernelRequest::RequestCompaction { session_id });
                }
            }

            KernelRequest::SetPolicy { session_id, policy } => {
                if let Some(entry) = self.sessions.get(&session_id) {
                    let _ = entry
                        .input_tx
                        .send(KernelRequest::SetPolicy { session_id, policy });
                }
            }

            KernelRequest::QuerySession { session_id } => {
                if let Some(entry) = self.sessions.get(&session_id) {
                    let _ = entry
                        .input_tx
                        .send(KernelRequest::QuerySession { session_id });
                }
            }

            KernelRequest::Shutdown => {
                // Send shutdown to all sessions
                for entry in self.sessions.values() {
                    let _ = entry.input_tx.send(KernelRequest::Shutdown);
                }
                return false; // Signal to stop the main loop
            }
        }
        true // Continue processing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_interfaces::protocol::SessionCreateConfig;
    use kernel_interfaces::types::*;

    #[test]
    fn router_register_tools_and_create_session() {
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let mut router = ConnectionRouter::new(event_tx, None, "echo".into());

        // Register a tool
        router.handle_request(KernelRequest::RegisterTools {
            tools: vec![ToolSchema {
                name: "file_read".into(),
                description: "Read a file".into(),
                capabilities: std::collections::HashSet::new(),
                schema: serde_json::json!({"type": "object"}),
                cost: TokenEstimate(100),
                relevance: RelevanceSignal {
                    keywords: vec![],
                    tags: vec![],
                },
            }],
        });

        // Create a session
        router.handle_request(KernelRequest::CreateSession {
            config: SessionCreateConfig {
                mode: SessionMode::Interactive,
                system_prompt: "Test.".into(),
                completion_config: CompletionConfig::default(),
                policy: kernel_interfaces::policy::Policy {
                    version: 1,
                    name: "allow-all".into(),
                    rules: vec![kernel_interfaces::policy::PolicyRule {
                        match_capabilities: vec!["*".into()],
                        action: kernel_interfaces::policy::PolicyAction::Allow,
                        scope_paths: vec![],
                        scope_commands: vec![],
                        except: vec![],
                    }],
                    resource_budgets: None,
                },
                resource_budget: ResourceBudget::default(),
                workspace: "/tmp/test".into(),
            },
        });

        // Should receive SessionCreated event
        let event = event_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(event, KernelEvent::SessionCreated { .. }));
    }

    #[test]
    fn router_shutdown_returns_false() {
        let (event_tx, _event_rx) = crossbeam_channel::unbounded();
        let mut router = ConnectionRouter::new(event_tx, None, "echo".into());

        let cont = router.handle_request(KernelRequest::Shutdown);
        assert!(!cont);
    }
}
