---
id: 0007-remote-sink
status: done
---

# Remote event sink: HTTP-POST session events to a remote endpoint

## Goal
Add a `HttpSink` implementation of `SessionEventSink` that POSTs each `SessionEvent` (as JSON) to a configured HTTP endpoint. This is **one-way remote audit / archival**: events flow out of the kernel to a remote store; the kernel itself still runs locally, and session state is still local. True remote session execution (session running on another machine, migration, resumption) remains spec 0008's problem.

The trait abstraction from spec 0003 already makes this a drop-in: `HttpSink` implements the same `SessionEventSink` as `NullSink` and `FileSink`, and the daemon can pick which one to instantiate.

## Context
- `crates/kernel-core/src/session_events.rs` — `SessionEventSink` trait and existing impls. `HttpSink` is a new sibling.
- `crates/kernel-daemon/src/router.rs` — the one place that constructs a sink at session-create time. This spec adds a second branch: if `AGENT_KERNEL_REMOTE_SINK_URL` is set, wrap the `FileSink` in a composite that writes to both file AND remote.
- The project has no HTTP client dependency today. Adding `ureq` (blocking HTTP, small, no async runtime) is the minimum-dependency path. Alternatives considered: `reqwest` (async, heavy), `curl` (C deps), hand-rolled `std::net::TcpStream` + manual HTTP (masochistic).
- Spec 0003 established the "best-effort write, surface via `failed_writes`" invariant. `HttpSink` keeps the same contract: a failed POST bumps a counter and logs to stderr, but never blocks the turn loop.

## Acceptance criteria
- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes
- [ ] **No new dependencies**. The HTTP POST is hand-rolled over `std::net::TcpStream` (~40 lines: URL parse, connect with timeout, write HTTP/1.1 request, read status line, drop the rest). Only `http://` is supported — `https://` URLs return an error at `HttpSink::new`. Rationale recorded in Notes: spec 0007 is audit-only, primary use case is localhost POST to a log aggregator, and adding a TLS-capable HTTP client would pull in ~15 transitive crates. A future spec can add an HTTPS-capable variant behind a feature flag when someone actually needs it.
- [ ] New struct `session_events::HttpSink` implementing `SessionEventSink`:
    - `HttpSink::new(session_id, endpoint_url, bearer_token: Option<String>) -> Self` — no network call at construction time.
    - `record(&mut self, event)` serializes the event to JSON and POSTs to `<endpoint_url>/events` with `Content-Type: application/json` and, if present, `Authorization: Bearer <token>`. Body is the single JSON event object (not wrapped).
    - Failed POSTs bump `failed_writes` and log to stderr, same as `FileSink`. The record call never blocks on network success.
    - Uses a short request timeout (2 seconds). A session shouldn't stall on a slow remote.
    - `HttpSink::failed_writes(&self) -> u64` and `HttpSink::endpoint(&self) -> &str` accessors.
- [ ] New struct `session_events::TeeSink<A, B>` — a generic composite that fans `record()` out to two inner sinks. Used by the daemon to send events to both the local `FileSink` and the remote `HttpSink` simultaneously. `session_id()` returns the id of the first sink. Drop-in `SessionEventSink` impl.
- [ ] Daemon wiring (`kernel-daemon/src/router.rs`):
    1. If `AGENT_KERNEL_REMOTE_SINK_URL` env var is set, construct an `HttpSink` with that URL and the optional `AGENT_KERNEL_REMOTE_SINK_TOKEN`.
    2. Construct the usual `FileSink` (or fall back to `NullSink` on failure, same as today).
    3. Wire both together via `TeeSink`. If remote sink construction failed in any observable way (URL parse?), log to stderr and skip it; file sink still runs.
    4. If `AGENT_KERNEL_REMOTE_SINK_URL` is **not** set, behavior is unchanged from spec 0006.
- [ ] New unit test `tee_sink_fans_out_to_both`: constructs a `TeeSink<VecSink, VecSink>`, records three events, asserts both inner sinks captured all three.
- [ ] New unit test `http_sink_records_to_mock_server`: spins up a minimal blocking HTTP server on an ephemeral port using `std::net::TcpListener` (no deps), constructs an `HttpSink` pointing at it, records an event, joins the server thread, asserts the server received a POST to `/events` with a body containing the event's identifying fields. This verifies the wire format and the fact that we're actually making a network call; mocks the server without pulling in a mock-http crate.
- [ ] New unit test `http_sink_failed_post_bumps_counter`: constructs an `HttpSink` pointing at an intentionally-closed port (`http://127.0.0.1:1` is the "port unreachable" convention), records an event, asserts `failed_writes()` > 0 and that the call didn't panic.
- [ ] `VecSink` (currently a test-only type inside `context.rs` tests) is promoted to a public helper in `session_events.rs` under `#[cfg(test)]` or behind a `test-util` feature, so both `context.rs` and `session_events.rs` tests can share it. If that's too invasive, leave a duplicate in the new tests — note the decision in Notes.

## Out of scope
- **Remote session execution** — the kernel still runs locally, only the audit stream goes remote. Session state lives in-memory and on the local file. Spec 0008 tackles actual remote execution.
- **Remote hydration** — `hydrate_from_events` still reads local files only. A remote-read path is a future spec.
- **Retry / backoff / queue** — a failed POST is logged and dropped. No in-memory queue of failed events, no retry loop. Reliable delivery is a much bigger feature.
- **Batching** — every event is its own POST. One-event-per-request is wasteful for high-volume sessions but simple; batching comes later if it matters.
- **TLS / cert pinning** — `ureq` does TLS via system roots. No pinning, no custom CA.
- **Authentication beyond bearer token** — no OAuth, no mTLS, no session tokens.
- **A remote server implementation** — this spec only does the client side. The test uses a 10-line mock server. In production you'd point at a logging service like Vector, Loki, or a custom sink.
- **Changing the FileSink behavior or the default_events_path resolution.**
- **Making the Tier-3 trait async.** `ureq` is blocking; the record call is synchronous. The 2-second timeout caps worst-case latency.

## Notes

- **Mid-spec change — no new HTTP dependency.** Original spec called for `ureq`. Rejected during execution: ureq with TLS would pull in ~15 transitive crates (rustls, webpki, ring, etc.) for a feature whose primary use case is POST to a localhost log aggregator. Hand-rolled `std::net::TcpStream`-based HTTP/1.1 client is ~40 lines, zero deps, http-only. HTTPS is an explicit future-spec slot, not a regression. This is documented in the spec AC above.
- **`SessionEventSink for Box<dyn SessionEventSink>` impl added** — needed so `TeeSink` can hold a `Box<dyn SessionEventSink>` as its primary (the daemon's local sink is runtime-selected between `FileSink` and `NullSink`). Without this impl the daemon would need a second layer of indirection or a newtype wrapper.
- **TeeSink is generic, not dynamic.** `TeeSink<A, B>` takes concrete types, not trait objects, so monomorphization gives it the same performance as hand-written fan-out. Used with `Box<dyn SessionEventSink>` on the primary (variable) and concrete `HttpSink` on the secondary.
- **HttpSink URL parsing is strict and eager.** `HttpSink::new` validates at construction, not at first event. `http://host[:port][/path]` only. Empty host rejected. Non-numeric port rejected. No query-string handling — the endpoint is assumed to be a bare path.
- **Request body is one JSON object per POST.** No batching, no newline-delimited, no wrapping envelope. Matches the JSONL format of `FileSink` (same wire format per event) so a log aggregator can ingest from both sinks identically.
- **Auth is bearer-token only.** Read from `AGENT_KERNEL_REMOTE_SINK_TOKEN` env var at session-create time (not at HttpSink-new time — the daemon reads the env and passes the value in).
- **Failure is fire-and-forget with a counter.** `post()` returns `io::Result`; failure bumps `failed_writes` and logs to stderr. No retry, no queue, no backoff. Audit is best-effort.
- **The "port 1 is unreachable" test** uses `http://127.0.0.1:1/events` because the OS treats port 1 as a valid address but (almost) never has anything listening. `connect_timeout` returns synchronously with `ConnectionRefused`. More reliable than picking a random high port that might coincidentally be in use.
- **`http_sink_records_to_mock_server`** spins up a one-shot TCP listener on `127.0.0.1:0` (ephemeral port) in a background thread, accepts one connection, reads the request, writes a 200 OK, and ships the request text back via a channel. 30 lines, no mock-http crate. The test asserts the request line, content-type header, auth header, and the event's identifying fields are present in the body.
- **`VecSink` decision**: declined to promote it to a public helper across modules. Left a near-duplicate in `context.rs` tests (pre-existing) and added a separate copy in `session_events.rs` tests for the TeeSink test. Reason: making it public even behind `#[cfg(test)]` requires a module structure change, and the duplication is ~20 lines total. Revisit if a third test module needs the same type.
- **Daemon wiring decision**: if the URL env var is set but malformed, the daemon logs an error and falls back to local-only. A bad URL should be noisy but not fatal — the session still runs. If the URL is valid but the endpoint is unreachable, the `HttpSink` itself handles the failure path at record time.
- **Verify loop**: `cargo fmt -- --check && cargo clippy && cargo test` all green. kernel-core: 74 unit (was 69, +5 — `tee_sink_fans_out_to_both`, `http_sink_new_rejects_non_http_urls`, `http_sink_new_parses_host_and_port`, `http_sink_records_to_mock_server`, `http_sink_failed_post_bumps_counter`). 15 e2e unchanged.
- **Judge pass skipped** per 0004 Notes rationale.
