use crate::types::CapabilitySet;
use serde::{Deserialize, Serialize};

/// An event from an external source (webhook, Slack, cron, file watcher, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalEvent {
    /// Source identifier: "github-webhook", "slack", "cron", "file-watcher"
    pub source: String,

    /// Event type: "pull_request.opened", "message", "daily", "file.changed"
    pub event_type: String,

    /// Raw event data.
    pub payload: serde_json::Value,
}

/// How external events enter the kernel. Channels are pluggable modules
/// that accept inbound connections and produce ExternalEvent values.
///
/// v0.1: No channel modules ship. The TUI acts as both frontend and channel.
/// The interface exists so v0.2 can add webhook support without modifying core.
pub trait ChannelInterface: Send {
    /// Start listening. Calls event_sink when events arrive.
    fn start(&self, event_sink: Box<dyn Fn(ExternalEvent) + Send>);

    /// Clean shutdown.
    fn stop(&self);

    /// What event types this channel can produce.
    fn produces(&self) -> Vec<String>;

    /// Capabilities this channel requires (e.g., net:listen:8080).
    fn capabilities(&self) -> CapabilitySet;
}
