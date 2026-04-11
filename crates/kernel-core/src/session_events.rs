//! Append-only session event stream (Tier-3 storage).
//!
//! The event stream is the **authoritative** record of a session. Every
//! user input, assistant response, tool exchange, and system message the
//! `ContextManager` sees is recorded as a `SessionEvent` before the
//! in-memory store is mutated. The stream is never modified after write
//! — compaction operates on the view (`ContextStore`), not on the event
//! stream.
//!
//! This module ships storage only. Model-accessible event queries, rewind,
//! and projection-based compaction are separate specs layered on top.
//!
//! The module is named `session_events` rather than `conversation_log`
//! because the stream is not restricted to conversation turns — future
//! specs will add permission decisions, policy changes, and compaction
//! events to it.

use kernel_interfaces::types::SessionId;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Returns the current time as milliseconds since UNIX epoch. Used by
/// `ContextManager` when constructing events.
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// No-op sink. Drops every event.
///
/// The default for tests and for kernel users that don't need a
/// persistent record.
pub struct NullSink {
    session_id: SessionId,
}

impl NullSink {
    pub fn new(session_id: SessionId) -> Self {
        Self { session_id }
    }
}

impl Default for NullSink {
    fn default() -> Self {
        Self::new(SessionId(0))
    }
}

impl SessionEventSink for NullSink {
    fn session_id(&self) -> SessionId {
        self.session_id
    }

    fn record(&mut self, _event: SessionEvent) {}
}

/// JSONL file sink. Appends one JSON object per line.
///
/// Opens the target file in append mode, creating parent directories
/// as needed. Each `record` call writes one line and flushes the
/// underlying writer so readers see a coherent file even mid-session.
pub struct FileSink {
    session_id: SessionId,
    writer: BufWriter<File>,
    path: PathBuf,
    failed_writes: u64,
}

impl FileSink {
    /// Create a new file sink. Creates parent directories if missing.
    /// Opens the file in append mode — an existing stream is preserved.
    pub fn new(session_id: SessionId, path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            session_id,
            writer: BufWriter::new(file),
            path,
            failed_writes: 0,
        })
    }

    /// Path of the event-stream file. Exposed for tests and debugging.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Number of writes that have failed since this sink was created.
    /// A non-zero value indicates disk or filesystem trouble; the stream
    /// is no longer authoritative.
    pub fn failed_writes(&self) -> u64 {
        self.failed_writes
    }
}

impl SessionEventSink for FileSink {
    fn session_id(&self) -> SessionId {
        self.session_id
    }

    fn record(&mut self, event: SessionEvent) {
        let line = match serde_json::to_string(&event) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "session_events: serialize failed for session {}: {}",
                    self.session_id.0, e
                );
                self.failed_writes += 1;
                return;
            }
        };
        if let Err(e) = writeln!(self.writer, "{line}") {
            eprintln!(
                "session_events: write failed for session {}: {}",
                self.session_id.0, e
            );
            self.failed_writes += 1;
            return;
        }
        if let Err(e) = self.writer.flush() {
            eprintln!(
                "session_events: flush failed for session {}: {}",
                self.session_id.0, e
            );
            self.failed_writes += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    fn read_lines(path: &Path) -> Vec<String> {
        let file = File::open(path).expect("open event stream for reading");
        BufReader::new(file)
            .lines()
            .collect::<Result<Vec<_>, _>>()
            .expect("read lines")
    }

    fn sample_user_input(turn_index: usize, text: &str) -> SessionEvent {
        SessionEvent::UserInput {
            timestamp_ms: 1_700_000_000_000,
            turn_index,
            text: text.into(),
        }
    }

    #[test]
    fn file_sink_writes_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session-7").join("events.jsonl");

        let mut sink = FileSink::new(SessionId(7), &path).unwrap();
        sink.record(sample_user_input(0, "hello"));
        sink.record(SessionEvent::AssistantResponse {
            timestamp_ms: 1_700_000_000_100,
            turn_index: 0,
            text: "hi there".into(),
        });
        sink.record(sample_user_input(1, "what files are here"));
        drop(sink);

        let lines = read_lines(&path);
        assert_eq!(lines.len(), 3);

        let parsed: Vec<SessionEvent> = lines
            .iter()
            .map(|l| serde_json::from_str(l).expect("parse line"))
            .collect();

        assert!(matches!(parsed[0], SessionEvent::UserInput { ref text, .. } if text == "hello"));
        assert!(
            matches!(parsed[1], SessionEvent::AssistantResponse { ref text, .. } if text == "hi there")
        );
        assert!(
            matches!(parsed[2], SessionEvent::UserInput { ref text, .. } if text == "what files are here")
        );
    }

    #[test]
    fn file_sink_appends_across_instances() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");

        {
            let mut sink = FileSink::new(SessionId(1), &path).unwrap();
            sink.record(sample_user_input(0, "first"));
            sink.record(sample_user_input(1, "second"));
        }

        {
            let mut sink = FileSink::new(SessionId(1), &path).unwrap();
            sink.record(sample_user_input(2, "third"));
        }

        let lines = read_lines(&path);
        assert_eq!(lines.len(), 3);
        let parsed: Vec<SessionEvent> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert!(matches!(parsed[0], SessionEvent::UserInput { ref text, .. } if text == "first"));
        assert!(matches!(parsed[1], SessionEvent::UserInput { ref text, .. } if text == "second"));
        assert!(matches!(parsed[2], SessionEvent::UserInput { ref text, .. } if text == "third"));
    }

    #[test]
    fn null_sink_drops_events() {
        let mut sink = NullSink::new(SessionId(42));
        assert_eq!(sink.session_id(), SessionId(42));
        sink.record(sample_user_input(0, "ignored"));
        sink.record(SessionEvent::SystemMessage {
            timestamp_ms: 0,
            turn_index: 0,
            text: "also ignored".into(),
        });
        // No observable side effect — this test mainly verifies the trait
        // is object-safe and the method signatures compile.
    }

    #[test]
    fn session_event_roundtrips_through_serde() {
        let event = SessionEvent::ToolExchange {
            timestamp_ms: 1_700_000_000_000,
            turn_index: 3,
            tool_name: "file_read".into(),
            input: serde_json::json!({"path": "src/main.rs"}),
            result: serde_json::json!({"content": "fn main() {}"}),
        };
        let s = serde_json::to_string(&event).unwrap();
        let back: SessionEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(event, back);
    }
}
