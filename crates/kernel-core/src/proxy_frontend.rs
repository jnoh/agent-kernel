//! A FrontendEvents implementation that bridges to an IPC channel.
//!
//! Instead of rendering to a terminal or IDE, `ProxyFrontend` serializes each
//! event as a `KernelEvent` and sends it to the distro over a channel.
//! Permission requests block until the distro responds.

use crossbeam_channel::{Receiver, Sender};
use kernel_interfaces::frontend::{
    CompactionSummary, FrontendEvents, KernelError, PermissionRequest,
};
use kernel_interfaces::protocol::{KernelEvent, RequestId};
use kernel_interfaces::tool::{ToolChunkStream, ToolOutput};
use kernel_interfaces::types::{Decision, SessionId, StreamChunk, TurnId};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Response from the distro for a permission request.
pub struct PermissionResponse {
    pub request_id: RequestId,
    pub decision: Decision,
}

/// A frontend that proxies all events over IPC channels.
pub struct ProxyFrontend {
    session_id: SessionId,
    event_tx: Sender<KernelEvent>,
    /// Channel to receive permission responses from the distro.
    permission_rx: Receiver<PermissionResponse>,
    /// Timeout for waiting on permission decisions.
    permission_timeout: Duration,
    /// Counter for generating unique request IDs.
    next_request_id: AtomicU64,
}

impl ProxyFrontend {
    pub fn new(
        session_id: SessionId,
        event_tx: Sender<KernelEvent>,
        permission_rx: Receiver<PermissionResponse>,
        permission_timeout: Duration,
    ) -> Self {
        Self {
            session_id,
            event_tx,
            permission_rx,
            permission_timeout,
            next_request_id: AtomicU64::new(0),
        }
    }

    fn next_request_id(&self) -> RequestId {
        RequestId(self.next_request_id.fetch_add(1, Ordering::Relaxed))
    }

    fn send(&self, event: KernelEvent) {
        // Best-effort send — if the channel is disconnected, the event is lost.
        // The turn loop will detect disconnection on the next tool execution.
        let _ = self.event_tx.send(event);
    }
}

impl FrontendEvents for ProxyFrontend {
    fn on_turn_start(&self, turn_id: TurnId) {
        self.send(KernelEvent::TurnStarted {
            session_id: self.session_id,
            turn_id,
        });
    }

    fn on_stream_chunk(&self, _chunk: &StreamChunk) {
        // Streaming not yet supported over IPC — will be added when
        // the provider supports streaming.
    }

    fn on_text(&self, text: &str) {
        self.send(KernelEvent::TextOutput {
            session_id: self.session_id,
            text: text.to_string(),
        });
    }

    fn on_tool_call(&self, tool_name: &str, input: &serde_json::Value) {
        self.send(KernelEvent::ToolCallStarted {
            session_id: self.session_id,
            tool_name: tool_name.to_string(),
            input: input.clone(),
        });
    }

    fn on_tool_output_chunk(&self, tool_name: &str, stream: ToolChunkStream, data: &str) {
        self.send(KernelEvent::ToolOutputChunk {
            session_id: self.session_id,
            tool_name: tool_name.to_string(),
            stream,
            data: data.to_string(),
        });
    }

    fn on_tool_result(&self, tool_name: &str, result: &ToolOutput) {
        // Since spec 0015, the kernel owns tool dispatch — the frontend
        // is not running tools itself, so it needs a direct notification
        // when a tool completes so it can render a display summary.
        self.send(KernelEvent::ToolCompleted {
            session_id: self.session_id,
            tool_name: tool_name.to_string(),
            result: result.result.clone(),
        });
    }

    fn on_permission_request(&self, request: &PermissionRequest) -> Decision {
        let request_id = self.next_request_id();

        self.send(KernelEvent::PermissionRequired {
            session_id: self.session_id,
            request_id,
            request: request.clone(),
        });

        // Block waiting for the distro's decision
        match self.permission_rx.recv_timeout(self.permission_timeout) {
            Ok(response) => response.decision,
            Err(_) => {
                // Timeout or disconnect — deny by default
                Decision::Deny("permission request timed out".into())
            }
        }
    }

    fn on_turn_end(&self, turn_id: TurnId) {
        self.send(KernelEvent::TurnEnded {
            session_id: self.session_id,
            turn_id,
            result: kernel_interfaces::protocol::TurnResultSummary {
                tool_calls_dispatched: 0,
                tool_calls_denied: 0,
                was_cancelled: false,
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        });
    }

    fn on_compaction(&self, summary: &CompactionSummary) {
        self.send(KernelEvent::CompactionHappened {
            session_id: self.session_id,
            summary: summary.clone(),
        });
    }

    fn on_workspace_changed(&self, _new_root: &Path) {
        // Workspace changes are handled at the session level, not via frontend events.
    }

    fn on_error(&self, error: &KernelError) {
        self.send(KernelEvent::Error {
            session_id: Some(self.session_id),
            error: error.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_frontend_sends_text_event() {
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (_perm_tx, perm_rx) = crossbeam_channel::unbounded();
        let frontend = ProxyFrontend::new(SessionId(1), event_tx, perm_rx, Duration::from_secs(10));

        frontend.on_text("Hello from the model");

        let event = event_rx.recv().unwrap();
        match event {
            KernelEvent::TextOutput { session_id, text } => {
                assert_eq!(session_id, SessionId(1));
                assert_eq!(text, "Hello from the model");
            }
            _ => panic!("unexpected event: {:?}", event),
        }
    }

    #[test]
    fn proxy_frontend_sends_turn_lifecycle() {
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (_perm_tx, perm_rx) = crossbeam_channel::unbounded();
        let frontend = ProxyFrontend::new(SessionId(0), event_tx, perm_rx, Duration::from_secs(10));

        frontend.on_turn_start(TurnId(5));
        frontend.on_turn_end(TurnId(5));

        let start = event_rx.recv().unwrap();
        assert!(matches!(
            start,
            KernelEvent::TurnStarted {
                turn_id: TurnId(5),
                ..
            }
        ));

        let end = event_rx.recv().unwrap();
        assert!(matches!(
            end,
            KernelEvent::TurnEnded {
                turn_id: TurnId(5),
                ..
            }
        ));
    }

    #[test]
    fn proxy_frontend_permission_round_trip() {
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (perm_tx, perm_rx) = crossbeam_channel::unbounded();
        let frontend = ProxyFrontend::new(SessionId(0), event_tx, perm_rx, Duration::from_secs(10));

        // Simulate distro responding to permission request
        let handle = std::thread::spawn(move || {
            let event = event_rx.recv().unwrap();
            match event {
                KernelEvent::PermissionRequired { request_id, .. } => {
                    perm_tx
                        .send(PermissionResponse {
                            request_id,
                            decision: Decision::Allow,
                        })
                        .unwrap();
                }
                _ => panic!("unexpected event"),
            }
        });

        let request = PermissionRequest {
            tool_name: "shell".into(),
            capabilities: vec!["shell:exec".into()],
            input_summary: "ls -la".into(),
        };
        let decision = frontend.on_permission_request(&request);
        assert_eq!(decision, Decision::Allow);

        handle.join().unwrap();
    }

    #[test]
    fn proxy_frontend_permission_timeout_denies() {
        let (event_tx, _event_rx) = crossbeam_channel::unbounded();
        let (_perm_tx, perm_rx) = crossbeam_channel::unbounded();
        let frontend = ProxyFrontend::new(
            SessionId(0),
            event_tx,
            perm_rx,
            Duration::from_millis(10), // very short timeout
        );

        let request = PermissionRequest {
            tool_name: "shell".into(),
            capabilities: vec!["shell:exec".into()],
            input_summary: "rm -rf /".into(),
        };
        let decision = frontend.on_permission_request(&request);
        assert!(matches!(decision, Decision::Deny(_)));
    }

    #[test]
    fn proxy_frontend_sends_error_event() {
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (_perm_tx, perm_rx) = crossbeam_channel::unbounded();
        let frontend = ProxyFrontend::new(SessionId(2), event_tx, perm_rx, Duration::from_secs(10));

        frontend.on_error(&KernelError {
            message: "something broke".into(),
            recoverable: true,
        });

        let event = event_rx.recv().unwrap();
        match event {
            KernelEvent::Error { session_id, error } => {
                assert_eq!(session_id, Some(SessionId(2)));
                assert_eq!(error.message, "something broke");
                assert!(error.recoverable);
            }
            _ => panic!("unexpected event"),
        }
    }
}
