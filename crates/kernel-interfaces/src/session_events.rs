//! Stable Tier-3 event stream abstraction.
//!
//! The types and trait in this module are the promotion target from
//! `kernel-core::session_events` — spec 0009 moved them here so that a
//! future distribution can implement its own sink (SQLite, Postgres,
//! cloud object store, etc.) while depending only on `kernel-interfaces`.
//!
//! Concrete impls (`NullSink`, `FileSink`, `HttpSink`, `TeeSink`) and
//! runtime helpers (`read_events_from_file`, `fingerprint_workspace`,
//! `default_events_path`, `now_millis`) stay in `kernel-core` because
//! they do filesystem I/O, subprocess work, env var reads, or clock
//! access — none of which belong in the stable API crate.

use crate::types::SessionId;
use serde::{Deserialize, Serialize};

/// Git-state fingerprint of a workspace at session-create time (spec 0008).
///
/// Recorded once in `SessionStarted` and compared against the current
/// workspace during hydration (if the caller asks). This is the minimum
/// viable workspace-sync primitive: it doesn't move files, but it lets a
/// replay refuse to run against the wrong commit.
///
/// Non-git workspaces produce a fingerprint with `commit=None,
/// branch=None, dirty=false` — the workspace is recorded by path only,
/// and match semantics treat non-git state as `Unknown`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceFingerprint {
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub dirty: bool,
    pub workspace_path: String,
}

/// Result of comparing two `WorkspaceFingerprint`s (spec 0008).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerprintMatch {
    /// Commits match and both are clean.
    Identical,
    /// Commits match but at least one side has uncommitted changes.
    SameCommitDirty,
    /// Commits differ.
    CommitMismatch,
    /// One or both lack git info — can't compare.
    Unknown,
}

impl WorkspaceFingerprint {
    pub fn matches(&self, other: &Self) -> FingerprintMatch {
        match (&self.commit, &other.commit) {
            (Some(a), Some(b)) if a == b => {
                if self.dirty || other.dirty {
                    FingerprintMatch::SameCommitDirty
                } else {
                    FingerprintMatch::Identical
                }
            }
            (Some(_), Some(_)) => FingerprintMatch::CommitMismatch,
            _ => FingerprintMatch::Unknown,
        }
    }
}

/// One event in the session's event stream.
///
/// Events are serialized one-per-line as JSON (JSONL). Each carries a
/// millisecond UNIX timestamp and the zero-based `turn_index` at the time
/// of the event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum SessionEvent {
    SessionStarted {
        timestamp_ms: u64,
        turn_index: usize,
        workspace: String,
        system_prompt: String,
        policy_name: String,
        /// Git fingerprint of the workspace at session-create time
        /// (spec 0008). `None` for older event files or when the
        /// caller didn't capture one.
        #[serde(default)]
        fingerprint: Option<WorkspaceFingerprint>,
    },
    UserInput {
        timestamp_ms: u64,
        turn_index: usize,
        text: String,
    },
    AssistantResponse {
        timestamp_ms: u64,
        turn_index: usize,
        text: String,
    },
    ToolExchange {
        timestamp_ms: u64,
        turn_index: usize,
        tool_name: String,
        input: serde_json::Value,
        result: serde_json::Value,
    },
    SystemMessage {
        timestamp_ms: u64,
        turn_index: usize,
        text: String,
    },
}

/// Append-only sink for `SessionEvent` values.
///
/// Implementations must be append-only and must not modify previously
/// written events. Writes are best-effort: failures are non-fatal and
/// surfaced via stderr + an internal counter, so a broken disk cannot
/// stall the turn loop.
pub trait SessionEventSink: Send {
    fn session_id(&self) -> SessionId;
    fn record(&mut self, event: SessionEvent);
}

/// Forwarding impl so `Box<dyn SessionEventSink>` is itself a
/// `SessionEventSink`. Required by composite sinks (e.g., `TeeSink`)
/// when either half needs to hold a trait object.
impl SessionEventSink for Box<dyn SessionEventSink> {
    fn session_id(&self) -> SessionId {
        (**self).session_id()
    }
    fn record(&mut self, event: SessionEvent) {
        (**self).record(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_event_roundtrip_lives_in_interfaces() {
        // Prove the moved types are usable with only kernel-interfaces
        // imports (no kernel-core touched by this test).
        let event = SessionEvent::UserInput {
            timestamp_ms: 1_700_000_000_000,
            turn_index: 0,
            text: "hello".into(),
        };
        let s = serde_json::to_string(&event).unwrap();
        let back: SessionEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn workspace_fingerprint_matches_semantics() {
        let a = WorkspaceFingerprint {
            commit: Some("abc".into()),
            branch: Some("main".into()),
            dirty: false,
            workspace_path: "/tmp/a".into(),
        };
        assert_eq!(a.matches(&a), FingerprintMatch::Identical);

        let mut dirty = a.clone();
        dirty.dirty = true;
        assert_eq!(a.matches(&dirty), FingerprintMatch::SameCommitDirty);

        let other = WorkspaceFingerprint {
            commit: Some("def".into()),
            ..a.clone()
        };
        assert_eq!(a.matches(&other), FingerprintMatch::CommitMismatch);

        let no_commit = WorkspaceFingerprint {
            commit: None,
            branch: None,
            dirty: false,
            workspace_path: "/tmp/none".into(),
        };
        assert_eq!(a.matches(&no_commit), FingerprintMatch::Unknown);
    }
}
