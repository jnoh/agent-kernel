//! The universal outer loop for driving agent sessions.
//!
//! `EventLoop` receives commands over a channel (user input, external events,
//! cancellation, policy changes) and drives the inner `TurnLoop` via `Session`.
//! Each `EventLoop` runs on its own thread — one per session.
//!
//! The interactive case is just an `EventLoop` where the event source is a
//! stdin reader thread pushing `AddInput` messages.

use crate::context::ContextConfig;
use crate::permission::PermissionEvaluator;
use crate::proxy_frontend::ProxyFrontend;
use crate::session::{PendingResult, Session};
#[cfg(test)]
use crate::session_events::NullSink;
use crate::session_events::SessionEventSink;
use crate::turn_loop::TurnLoop;
use crossbeam_channel::{Receiver, Sender};
use kernel_interfaces::frontend::{KernelError, SessionControl};
use kernel_interfaces::protocol::{
    KernelEvent, KernelRequest, SessionCreateConfig, TurnResultSummary,
};
use kernel_interfaces::provider::ProviderInterface;
use kernel_interfaces::tool::ToolRegistration;
use kernel_interfaces::types::SessionId;
use std::path::PathBuf;

/// Configuration for an EventLoop, built from protocol messages.
pub struct EventLoopConfig {
    pub session_id: SessionId,
    pub session_create: SessionCreateConfig,
    pub tools: Vec<Box<dyn ToolRegistration>>,
    pub provider: Box<dyn ProviderInterface + Send>,
    pub frontend: ProxyFrontend,
    /// Append-only session event sink. Daemon injects a `FileSink`;
    /// tests use a `NullSink`.
    pub events: Box<dyn SessionEventSink>,
}

impl EventLoopConfig {
    /// Helper for tests: builds a config with a `NullSink`. Keeps the
    /// test code that constructs `EventLoopConfig` from sprouting a
    /// `events: Box::new(NullSink::default())` line in every call site.
    #[cfg(test)]
    pub fn with_null_sink(
        session_id: SessionId,
        session_create: SessionCreateConfig,
        tools: Vec<Box<dyn ToolRegistration>>,
        provider: Box<dyn ProviderInterface + Send>,
        frontend: ProxyFrontend,
    ) -> Self {
        Self {
            session_id,
            session_create,
            tools,
            provider,
            frontend,
            events: Box::new(NullSink::new(session_id)),
        }
    }
}

/// The event loop — receives commands and drives the session.
pub struct EventLoop {
    session_id: SessionId,
    session: Session,
    provider: Box<dyn ProviderInterface + Send>,
    frontend: ProxyFrontend,
    input_rx: Receiver<KernelRequest>,
    output_tx: Sender<KernelEvent>,
}

impl EventLoop {
    /// Create an EventLoop from config and channels.
    pub fn new(
        config: EventLoopConfig,
        input_rx: Receiver<KernelRequest>,
        output_tx: Sender<KernelEvent>,
    ) -> Self {
        let sc = config.session_create;
        let context_config = ContextConfig {
            context_window: 200_000, // TODO: make configurable via protocol
            ..Default::default()
        };

        let mut context = crate::context::ContextManager::with_event_sink(
            context_config,
            sc.system_prompt.clone(),
            config.events,
        );
        let fingerprint =
            crate::session_events::fingerprint_workspace(std::path::Path::new(&sc.workspace));
        context.record_session_started(
            sc.workspace.clone(),
            sc.policy.name.clone(),
            Some(fingerprint),
        );

        let permission = PermissionEvaluator::new(sc.policy);
        let turn_loop = TurnLoop::new(
            sc.completion_config,
            sc.resource_budget.max_tool_invocations_per_turn,
        );
        let max_tokens = sc.resource_budget.max_tokens_per_session;

        let session = Session::new(
            config.session_id,
            sc.mode,
            PathBuf::from(&sc.workspace),
            context,
            permission,
            turn_loop,
            config.tools,
            max_tokens,
        );

        Self {
            session_id: config.session_id,
            session,
            provider: config.provider,
            frontend: config.frontend,
            input_rx,
            output_tx,
        }
    }

    /// Run the event loop until Shutdown or channel disconnect.
    pub fn run(&mut self) {
        loop {
            let request = match self.input_rx.recv() {
                Ok(req) => req,
                Err(_) => break, // Channel disconnected
            };

            match request {
                KernelRequest::AddInput { text, .. } => {
                    self.session.add_user_input(text);
                    self.run_until_yield();
                }
                KernelRequest::DeliverEvent { event, .. } => {
                    self.session.deliver(PendingResult::ExternalEvent {
                        source: event.source,
                        event_type: event.event_type,
                        summary: event.payload.to_string(),
                    });
                }
                KernelRequest::CancelTurn { .. } => {
                    self.session.cancel();
                }
                KernelRequest::SetPolicy { policy, .. } => {
                    SessionControl::set_policy(&mut self.session, policy);
                }
                KernelRequest::RequestCompaction { .. } => {
                    match self.session.request_compaction(&*self.provider) {
                        Ok(freed) => {
                            let _ = self.output_tx.send(KernelEvent::CompactionHappened {
                                session_id: self.session_id,
                                summary: kernel_interfaces::frontend::CompactionSummary {
                                    turns_before: self.session.context().turn_count(),
                                    turns_after: self.session.context().turn_count(),
                                    tokens_freed: freed,
                                },
                            });
                        }
                        Err(msg) => {
                            let _ = self.output_tx.send(KernelEvent::Error {
                                session_id: Some(self.session_id),
                                error: KernelError {
                                    message: msg,
                                    recoverable: true,
                                },
                            });
                        }
                    }
                }
                KernelRequest::QuerySession { .. } => {
                    let _ = self.output_tx.send(KernelEvent::SessionStatus {
                        session_id: self.session_id,
                        tokens_used: self.session.tokens_used(),
                        utilization: self.session.context_utilization(),
                        turn_count: self.session.turn_count(),
                    });
                }
                KernelRequest::Shutdown => break,
                // Other request types are handled by the connection router, not the event loop
                _ => {}
            }
        }
    }

    /// Run turns until the model yields (no more tool calls) or an error occurs.
    fn run_until_yield(&mut self) {
        loop {
            let result = self.session.run_turn(&*self.provider, &self.frontend);
            match result {
                Ok(r) => {
                    let summary = TurnResultSummary {
                        tool_calls_dispatched: r.tool_calls_dispatched,
                        tool_calls_denied: r.tool_calls_denied,
                        was_cancelled: r.was_cancelled,
                        input_tokens: r.usage.input_tokens,
                        output_tokens: r.usage.output_tokens,
                        cache_creation_input_tokens: r.usage.cache_creation_input_tokens,
                        cache_read_input_tokens: r.usage.cache_read_input_tokens,
                    };
                    if !r.continues {
                        let _ = self.output_tx.send(KernelEvent::TurnEnded {
                            session_id: self.session_id,
                            turn_id: r.turn_id,
                            result: summary,
                        });
                        break;
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    let _ = self.output_tx.send(KernelEvent::Error {
                        session_id: Some(self.session_id),
                        error: KernelError {
                            message: msg,
                            recoverable: true,
                        },
                    });
                    // Send a TurnEnded so the frontend knows the turn
                    // is over and re-enables input.
                    let _ = self.output_tx.send(KernelEvent::TurnEnded {
                        session_id: self.session_id,
                        turn_id: kernel_interfaces::types::TurnId(0),
                        result: TurnResultSummary {
                            tool_calls_dispatched: 0,
                            tool_calls_denied: 0,
                            was_cancelled: false,
                            input_tokens: 0,
                            output_tokens: 0,
                            cache_creation_input_tokens: 0,
                            cache_read_input_tokens: 0,
                        },
                    });
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use kernel_interfaces::types::*;
    use std::time::Duration;

    fn test_event_loop(
        provider: Box<dyn ProviderInterface + Send>,
        tools: Vec<Box<dyn ToolRegistration>>,
    ) -> (
        Sender<KernelRequest>,
        Receiver<KernelEvent>,
        std::thread::JoinHandle<()>,
    ) {
        let (input_tx, input_rx) = crossbeam_channel::unbounded();
        let (output_tx, output_rx) = crossbeam_channel::unbounded();
        let (_perm_tx, perm_rx) = crossbeam_channel::unbounded();

        let session_id = SessionId(0);
        let event_tx_for_frontend = output_tx.clone();
        let frontend = ProxyFrontend::new(
            session_id,
            event_tx_for_frontend,
            perm_rx,
            Duration::from_secs(10),
        );

        let config = EventLoopConfig::with_null_sink(
            session_id,
            SessionCreateConfig {
                mode: SessionMode::Interactive,
                system_prompt: "You are helpful.".into(),
                completion_config: CompletionConfig::default(),
                policy: allow_all_policy(),
                resource_budget: ResourceBudget::default(),
                workspace: "/tmp/test".into(),
            },
            tools,
            provider,
            frontend,
        );

        let mut event_loop = EventLoop::new(config, input_rx, output_tx);
        let handle = std::thread::spawn(move || {
            event_loop.run();
        });

        (input_tx, output_rx, handle)
    }

    #[test]
    fn event_loop_text_only_turn() {
        let provider = Box::new(FakeProvider {
            response: text_response("Hello!"),
        });

        let (input_tx, output_rx, handle) = test_event_loop(provider, vec![]);

        input_tx
            .send(KernelRequest::AddInput {
                session_id: SessionId(0),
                text: "Hi".into(),
            })
            .unwrap();

        // Collect events until TurnEnded
        let mut got_text = false;
        loop {
            let event = output_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            match event {
                KernelEvent::TextOutput { text, .. } => {
                    assert_eq!(text, "Hello!");
                    got_text = true;
                }
                KernelEvent::TurnEnded { result, .. } => {
                    assert_eq!(result.tool_calls_dispatched, 0);
                    break;
                }
                KernelEvent::TurnStarted { .. } => {} // expected
                _ => {}
            }
        }

        assert!(got_text);

        input_tx.send(KernelRequest::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn event_loop_query_session() {
        let provider = Box::new(FakeProvider {
            response: text_response("Hi"),
        });

        let (input_tx, output_rx, handle) = test_event_loop(provider, vec![]);

        input_tx
            .send(KernelRequest::QuerySession {
                session_id: SessionId(0),
            })
            .unwrap();

        let event = output_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        match event {
            KernelEvent::SessionStatus {
                tokens_used,
                turn_count,
                ..
            } => {
                assert!(tokens_used > 0); // system prompt tokens
                assert_eq!(turn_count, 0);
            }
            _ => panic!("expected SessionStatus, got {:?}", event),
        }

        input_tx.send(KernelRequest::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn event_loop_shutdown() {
        let provider = Box::new(FakeProvider {
            response: text_response("Hi"),
        });

        let (input_tx, _output_rx, handle) = test_event_loop(provider, vec![]);

        input_tx.send(KernelRequest::Shutdown).unwrap();
        handle.join().unwrap(); // Should exit cleanly
    }

    #[test]
    fn event_loop_channel_disconnect_exits() {
        let provider = Box::new(FakeProvider {
            response: text_response("Hi"),
        });

        let (input_tx, _output_rx, handle) = test_event_loop(provider, vec![]);

        drop(input_tx); // Disconnect the channel
        handle.join().unwrap(); // Should exit cleanly
    }
}
