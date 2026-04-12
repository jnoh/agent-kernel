//! Connection router — dispatches protocol messages between the socket and
//! per-session EventLoops.
//!
//! As of spec 0015, tool dispatch is entirely kernel-side. The router does
//! not accept tool schemas from the distro; instead it snapshots a fresh
//! tool list from the shared `ToolsetPool` at every `CreateSession` call.

use crate::toolset_pool::ToolsetPool;
use crossbeam_channel::Sender;
use kernel_core::event_loop::{EventLoop, EventLoopConfig};
use kernel_core::proxy_frontend::ProxyFrontend;
use kernel_core::session_events::{
    FileSink, HttpSink, NullSink, SessionEventSink, TeeSink, default_events_path,
};
use kernel_interfaces::protocol::{KernelEvent, KernelRequest};
use kernel_interfaces::provider::ProviderInterface;
use kernel_interfaces::types::SessionId;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::manifest::ProviderFactory;

/// Per-session state tracked by the router.
struct SessionEntry {
    input_tx: Sender<KernelRequest>,
    /// Permission response channel for the ProxyFrontend.
    permission_tx: Sender<kernel_core::proxy_frontend::PermissionResponse>,
}

/// The connection router manages sessions, routes messages, and owns the
/// provider factory + toolset pool.
pub struct ConnectionRouter {
    sessions: HashMap<SessionId, SessionEntry>,
    next_session_id: u64,
    /// Channel for outgoing events to the socket writer.
    event_tx: Sender<KernelEvent>,
    /// Provider factory. Called once per session-create to produce a
    /// fresh `Box<dyn ProviderInterface + Send>`. Source of truth is
    /// the distribution manifest loaded in `main.rs`.
    provider_factory: ProviderFactory,
    /// Shared toolset pool. Daemon-scoped; each `CreateSession` snapshots
    /// a fresh tool list via `pool.tools_for_session()`.
    toolset_pool: Arc<ToolsetPool>,
}

impl ConnectionRouter {
    pub fn new(
        event_tx: Sender<KernelEvent>,
        provider_factory: ProviderFactory,
        toolset_pool: Arc<ToolsetPool>,
    ) -> Self {
        Self {
            sessions: HashMap::new(),
            next_session_id: 0,
            event_tx,
            provider_factory,
            toolset_pool,
        }
    }

    /// Handle an incoming request from the distro.
    pub fn handle_request(&mut self, request: KernelRequest) -> bool {
        match request {
            KernelRequest::CreateSession { config } => {
                let session_id = SessionId(self.next_session_id);
                let tools = self.toolset_pool.tools_for_session();
                eprintln!(
                    "  creating session {:?} with {} tools (from pool)",
                    session_id,
                    tools.len()
                );
                self.next_session_id += 1;

                // Create per-session channels
                let (input_tx, input_rx) = crossbeam_channel::unbounded();
                let (permission_tx, permission_rx) = crossbeam_channel::unbounded();

                // Build a fresh provider for this session from the
                // manifest-derived factory.
                let provider: Box<dyn ProviderInterface + Send> = (self.provider_factory)();

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
    use kernel_interfaces::manifest::ToolsetEntry;
    use kernel_interfaces::protocol::SessionCreateConfig;
    use kernel_interfaces::types::*;
    use kernel_providers::EchoProvider;

    fn echo_factory() -> ProviderFactory {
        Arc::new(|| Box::new(EchoProvider) as Box<dyn ProviderInterface + Send>)
    }

    fn empty_pool() -> Arc<ToolsetPool> {
        Arc::new(
            ToolsetPool::build(
                &[] as &[ToolsetEntry],
                &crate::toolset_pool::default_registry(),
            )
            .expect("empty pool"),
        )
    }

    #[test]
    fn router_create_session_without_register_tools() {
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let mut router = ConnectionRouter::new(event_tx, echo_factory(), empty_pool());

        // Create a session directly — no RegisterTools step required.
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
        let mut router = ConnectionRouter::new(event_tx, echo_factory(), empty_pool());

        let cont = router.handle_request(KernelRequest::Shutdown);
        assert!(!cont);
    }
}
