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
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// Forwarding impl so `Box<dyn SessionEventSink>` is itself a
/// `SessionEventSink`. Required by `TeeSink<A, B>` when either half
/// needs to hold a trait object (e.g., the daemon's local sink,
/// which is `FileSink` or `NullSink` depending on runtime state).
impl SessionEventSink for Box<dyn SessionEventSink> {
    fn session_id(&self) -> SessionId {
        (**self).session_id()
    }
    fn record(&mut self, event: SessionEvent) {
        (**self).record(event);
    }
}

/// Read a JSONL event file into a vector. Each line is one event.
///
/// Fails with `io::ErrorKind::InvalidData` on the first unparseable line,
/// including the line number in the error message. An empty file returns
/// `Ok(vec![])`.
pub fn read_events_from_file(path: impl AsRef<Path>) -> std::io::Result<Vec<SessionEvent>> {
    let file = File::open(path.as_ref())?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<SessionEvent>(&line) {
            Ok(event) => events.push(event),
            Err(e) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("line {}: {e}", idx + 1),
                ));
            }
        }
    }
    Ok(events)
}

/// Resolve the default on-disk path for a session's events file.
///
/// Base directory is picked in this order:
/// 1. `$AGENT_KERNEL_HOME` if set
/// 2. `$HOME/.agent-kernel` (Unix default)
/// 3. `./.agent-kernel` (last-resort fallback — CI without HOME)
///
/// The final path is `<base>/sessions/{id}/events.jsonl`. Parent
/// directories are NOT created here — `FileSink::new` handles that.
pub fn default_events_path(session_id: SessionId) -> PathBuf {
    let base: PathBuf = if let Ok(override_path) = std::env::var("AGENT_KERNEL_HOME") {
        PathBuf::from(override_path)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".agent-kernel")
    } else {
        PathBuf::from(".agent-kernel")
    };
    base.join("sessions")
        .join(format!("{}", session_id.0))
        .join("events.jsonl")
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

/// Composite sink that fans `record` calls out to two inner sinks.
/// Used by the daemon (spec 0007) to write every event to a local
/// `FileSink` and a remote `HttpSink` simultaneously.
pub struct TeeSink<A: SessionEventSink, B: SessionEventSink> {
    primary: A,
    secondary: B,
}

impl<A: SessionEventSink, B: SessionEventSink> TeeSink<A, B> {
    pub fn new(primary: A, secondary: B) -> Self {
        Self { primary, secondary }
    }
}

impl<A: SessionEventSink, B: SessionEventSink> SessionEventSink for TeeSink<A, B> {
    fn session_id(&self) -> SessionId {
        self.primary.session_id()
    }

    fn record(&mut self, event: SessionEvent) {
        // Clone so both sinks get the event. `SessionEvent` is `Clone`.
        self.primary.record(event.clone());
        self.secondary.record(event);
    }
}

/// HTTP POST sink. Fires-and-forgets each event to a configured
/// `http://` endpoint.
///
/// **Audit-only** — this sink is best-effort, synchronous, http-only
/// (no TLS), no retry, no queue. Per the spec 0007 rationale, adding a
/// TLS-capable HTTP client would pull in ~15 transitive crates for a
/// feature whose primary use case is localhost POST to a log
/// aggregator. A future spec can add an HTTPS variant.
///
/// `record()` blocks on the network call up to the request timeout
/// (2 seconds by default). A slow or unreachable endpoint bumps
/// `failed_writes` and returns; it never panics and never stalls the
/// turn loop indefinitely.
pub struct HttpSink {
    session_id: SessionId,
    host: String,
    port: u16,
    path: String,
    bearer_token: Option<String>,
    timeout: Duration,
    failed_writes: u64,
}

impl HttpSink {
    /// Construct an HTTP sink. Parses the endpoint URL eagerly so
    /// config errors are caught at session creation rather than at
    /// the first event. Only `http://host[:port][/path]` URLs are
    /// accepted.
    pub fn new(
        session_id: SessionId,
        endpoint_url: &str,
        bearer_token: Option<String>,
    ) -> Result<Self, String> {
        let rest = endpoint_url
            .strip_prefix("http://")
            .ok_or_else(|| format!("HttpSink only accepts http:// URLs, got {endpoint_url}"))?;
        let (host_port, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = match host_port.rfind(':') {
            Some(i) => {
                let port: u16 = host_port[i + 1..]
                    .parse()
                    .map_err(|_| format!("invalid port in URL: {endpoint_url}"))?;
                (host_port[..i].to_string(), port)
            }
            None => (host_port.to_string(), 80),
        };
        if host.is_empty() {
            return Err(format!("empty host in URL: {endpoint_url}"));
        }
        Ok(Self {
            session_id,
            host,
            port,
            path: path.to_string(),
            bearer_token,
            timeout: Duration::from_secs(2),
            failed_writes: 0,
        })
    }

    pub fn failed_writes(&self) -> u64 {
        self.failed_writes
    }

    pub fn endpoint(&self) -> String {
        format!("http://{}:{}{}", self.host, self.port, self.path)
    }

    fn post(&self, body: &str) -> std::io::Result<()> {
        let addr = (self.host.as_str(), self.port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "no addresses resolved")
            })?;
        let mut stream = TcpStream::connect_timeout(&addr, self.timeout)?;
        stream.set_write_timeout(Some(self.timeout))?;
        stream.set_read_timeout(Some(self.timeout))?;

        let auth_header = match &self.bearer_token {
            Some(t) => format!("Authorization: Bearer {t}\r\n"),
            None => String::new(),
        };
        let request = format!(
            "POST {path} HTTP/1.1\r\n\
             Host: {host}:{port}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {len}\r\n\
             {auth_header}\
             Connection: close\r\n\
             \r\n\
             {body}",
            path = self.path,
            host = self.host,
            port = self.port,
            len = body.len(),
            auth_header = auth_header,
            body = body,
        );
        stream.write_all(request.as_bytes())?;
        stream.flush()?;

        // Read the status line so the server gets to finish writing
        // before we drop the socket. We don't parse it; any 2xx or 3xx
        // is fine for audit-only use.
        let mut buf = [0u8; 512];
        let _ = stream.read(&mut buf);
        Ok(())
    }
}

impl SessionEventSink for HttpSink {
    fn session_id(&self) -> SessionId {
        self.session_id
    }

    fn record(&mut self, event: SessionEvent) {
        let body = match serde_json::to_string(&event) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "session_events: HttpSink serialize failed for session {}: {}",
                    self.session_id.0, e
                );
                self.failed_writes += 1;
                return;
            }
        };
        if let Err(e) = self.post(&body) {
            eprintln!(
                "session_events: HttpSink POST to {} failed for session {}: {}",
                self.endpoint(),
                self.session_id.0,
                e
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

    /// Single test covering all three branches of `default_events_path`.
    /// Intentionally one test rather than three: cargo test runs tests
    /// in parallel, and env var mutation is process-wide. Serializing
    /// all the cases into one sequential test function removes the
    /// race risk without pulling in `serial_test`.
    #[test]
    fn default_events_path_resolves_base_dir() {
        // Save whatever is currently in these vars so the test restores
        // them on exit. This isn't bulletproof against panics, but tests
        // shouldn't be mutating process env in the first place — this is
        // the least-bad option.
        let saved_override = std::env::var("AGENT_KERNEL_HOME").ok();
        let saved_home = std::env::var("HOME").ok();

        // Branch 1: AGENT_KERNEL_HOME wins.
        unsafe {
            std::env::set_var("AGENT_KERNEL_HOME", "/tmp/ak-test-override");
        }
        let p = default_events_path(SessionId(7));
        assert!(
            p.starts_with("/tmp/ak-test-override"),
            "expected override prefix, got: {}",
            p.display()
        );
        assert!(p.ends_with("sessions/7/events.jsonl"));

        // Branch 2: AGENT_KERNEL_HOME unset, HOME wins.
        unsafe {
            std::env::remove_var("AGENT_KERNEL_HOME");
            std::env::set_var("HOME", "/tmp/ak-test-home");
        }
        let p = default_events_path(SessionId(8));
        assert!(
            p.starts_with("/tmp/ak-test-home/.agent-kernel"),
            "expected $HOME/.agent-kernel prefix, got: {}",
            p.display()
        );
        assert!(p.ends_with("sessions/8/events.jsonl"));

        // Branch 3: neither set — falls back to `./.agent-kernel`.
        unsafe {
            std::env::remove_var("AGENT_KERNEL_HOME");
            std::env::remove_var("HOME");
        }
        let p = default_events_path(SessionId(9));
        assert!(
            p.starts_with(".agent-kernel"),
            "expected local fallback, got: {}",
            p.display()
        );
        assert!(p.ends_with("sessions/9/events.jsonl"));

        // Restore.
        unsafe {
            match saved_override {
                Some(v) => std::env::set_var("AGENT_KERNEL_HOME", v),
                None => std::env::remove_var("AGENT_KERNEL_HOME"),
            }
            match saved_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn read_events_from_file_roundtrips_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        let mut sink = FileSink::new(SessionId(0), &path).unwrap();
        sink.record(sample_user_input(0, "first"));
        sink.record(SessionEvent::AssistantResponse {
            timestamp_ms: 0,
            turn_index: 0,
            text: "hello".into(),
        });
        sink.record(sample_user_input(1, "second"));
        drop(sink);

        let events = read_events_from_file(&path).expect("read events");
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], SessionEvent::UserInput { ref text, .. } if text == "first"));
        assert!(
            matches!(events[1], SessionEvent::AssistantResponse { ref text, .. } if text == "hello")
        );
        assert!(matches!(events[2], SessionEvent::UserInput { ref text, .. } if text == "second"));
    }

    #[test]
    fn read_events_malformed_line_errors() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"UserInput","timestamp_ms":0,"turn_index":0,"text":"ok"}}"#
        )
        .unwrap();
        writeln!(f, "this is not json").unwrap();
        drop(f);

        let err = read_events_from_file(&path).expect_err("should fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("line 2"),
            "expected line number in error, got: {err}"
        );
    }

    /// Shared-memory test sink for assertions on what was recorded.
    /// Publicly available from this module's test scope so the TeeSink
    /// test can use it as both inner sinks.
    #[derive(Clone)]
    struct VecSink {
        session_id: SessionId,
        events: std::sync::Arc<std::sync::Mutex<Vec<SessionEvent>>>,
    }

    impl VecSink {
        fn new(session_id: SessionId) -> Self {
            Self {
                session_id,
                events: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }
        fn snapshot(&self) -> Vec<SessionEvent> {
            self.events.lock().unwrap().clone()
        }
    }

    impl SessionEventSink for VecSink {
        fn session_id(&self) -> SessionId {
            self.session_id
        }
        fn record(&mut self, event: SessionEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[test]
    fn tee_sink_fans_out_to_both() {
        let a = VecSink::new(SessionId(1));
        let b = VecSink::new(SessionId(2));
        let a_handle = a.clone();
        let b_handle = b.clone();
        let mut tee = TeeSink::new(a, b);

        for i in 0..3 {
            tee.record(SessionEvent::UserInput {
                timestamp_ms: i,
                turn_index: i as usize,
                text: format!("msg {i}"),
            });
        }

        let a_events = a_handle.snapshot();
        let b_events = b_handle.snapshot();
        assert_eq!(a_events.len(), 3);
        assert_eq!(b_events.len(), 3);
        assert_eq!(a_events, b_events);
        // TeeSink's session_id comes from the primary (first) inner.
        assert_eq!(tee.session_id(), SessionId(1));
    }

    #[test]
    fn http_sink_new_rejects_non_http_urls() {
        match HttpSink::new(SessionId(0), "https://example.com/events", None) {
            Ok(_) => panic!("expected error"),
            Err(e) => assert!(e.contains("http://")),
        }
        match HttpSink::new(SessionId(0), "ftp://example.com", None) {
            Ok(_) => panic!("expected error"),
            Err(e) => assert!(e.contains("http://")),
        }
    }

    #[test]
    fn http_sink_new_parses_host_and_port() {
        let sink = HttpSink::new(SessionId(0), "http://example.com/events", None).unwrap();
        assert_eq!(sink.endpoint(), "http://example.com:80/events");

        let sink = HttpSink::new(SessionId(0), "http://localhost:9000/x", None).unwrap();
        assert_eq!(sink.endpoint(), "http://localhost:9000/x");

        let sink = HttpSink::new(SessionId(0), "http://127.0.0.1:3000", None).unwrap();
        assert_eq!(sink.endpoint(), "http://127.0.0.1:3000/");
    }

    #[test]
    fn http_sink_records_to_mock_server() {
        use std::net::TcpListener;
        use std::sync::mpsc;

        // Spin up a minimal one-shot HTTP server in a background thread.
        // Binds to :0 for an ephemeral port so parallel tests don't clash.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel::<String>();

        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4096];
            let n = sock.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            let _ = tx.send(request);
        });

        let url = format!("http://127.0.0.1:{}/events", addr.port());
        let mut sink = HttpSink::new(SessionId(42), &url, Some("secret-token".into())).unwrap();
        sink.record(SessionEvent::UserInput {
            timestamp_ms: 1_700_000_000_000,
            turn_index: 0,
            text: "hello-from-test".into(),
        });

        server.join().expect("server thread");
        let request = rx.recv().expect("server received a request");

        assert!(request.starts_with("POST /events HTTP/1.1"));
        assert!(request.contains("Content-Type: application/json"));
        assert!(request.contains("Authorization: Bearer secret-token"));
        assert!(request.contains("hello-from-test"));
        assert!(request.contains("UserInput"));
        assert_eq!(sink.failed_writes(), 0);
    }

    #[test]
    fn http_sink_failed_post_bumps_counter() {
        // Port 1 on localhost is reserved and (almost) always unreachable
        // or refused — the OS treats it as a fine host:port pair but no
        // process listens there. A connection attempt returns an error
        // synchronously, which is exactly what we want to exercise.
        let mut sink = HttpSink::new(SessionId(0), "http://127.0.0.1:1/events", None).unwrap();
        sink.record(SessionEvent::UserInput {
            timestamp_ms: 0,
            turn_index: 0,
            text: "will fail".into(),
        });
        assert!(sink.failed_writes() >= 1);
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
