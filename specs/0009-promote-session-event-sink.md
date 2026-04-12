---
id: 0009-promote-session-event-sink
status: done
---

# Promote SessionEventSink (and friends) to kernel-interfaces

## Goal
Move the `SessionEventSink` trait, the `SessionEvent` enum, and the `WorkspaceFingerprint` / `FingerprintMatch` types from `kernel-core` to `kernel-interfaces`. This puts the Tier-3 storage abstraction on the stable API surface, so a future distribution can implement its own sink (SQLite, Postgres, cloud object store, etc.) by depending only on `kernel-interfaces` — the existing "distributions only depend on the stable crate" invariant.

Keep all concrete impls (`NullSink`, `FileSink`, `HttpSink`, `TeeSink`) and all runtime helpers (`read_events_from_file`, `fingerprint_workspace`, `default_events_path`, `now_millis`) in `kernel-core`. Only the abstraction and the shared data types move. This is consistent with the existing pattern: traits/types in interfaces, impls in core.

## Context
- `crates/kernel-core/src/session_events.rs` — source of truth today. Contains trait + enum + types + 4 impls + 3 helpers.
- `crates/kernel-interfaces/src/lib.rs` — new module `session_events` gets added here.
- `crates/kernel-core/src/lib.rs` — the core module re-exports `pub use` from interfaces for the moved items so in-core consumers keep working without import rewrites.
- `crates/kernel-core/src/context.rs`, `event_loop.rs`, `session.rs` — all currently `use crate::session_events::{...}`. After the move, these still work if `kernel-core::session_events` re-exports the moved items.
- `crates/kernel-daemon/src/router.rs` — uses `kernel_core::session_events::{FileSink, ...}`. Unchanged if re-exports are in place.
- Spec 0003 Notes explicitly called out "promotion to `kernel-interfaces` is a follow-up if/when a second distribution consumes it." The modularity audit in this session identified this as the single biggest current gap.

## Acceptance criteria
- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes
- [ ] New file `crates/kernel-interfaces/src/session_events.rs` containing:
    - `pub trait SessionEventSink: Send` (with `session_id()` and `record()`)
    - `pub enum SessionEvent` (all 5 variants with serde derive)
    - `pub struct WorkspaceFingerprint` (with serde derive + `matches()` method + `FingerprintMatch` return)
    - `pub enum FingerprintMatch`
    - `impl SessionEventSink for Box<dyn SessionEventSink>` forwarder
  No runtime impls, no filesystem I/O, no subprocess calls, no env var reads.
- [ ] `crates/kernel-interfaces/src/lib.rs` declares the new module.
- [ ] `crates/kernel-core/src/session_events.rs` has the moved types removed but re-exports them via `pub use kernel_interfaces::session_events::{...}` so every existing in-core consumer (`context.rs`, `event_loop.rs`, `session.rs`, the daemon, tests) keeps its `use crate::session_events::...` paths unchanged. The file still contains `NullSink`, `FileSink`, `HttpSink`, `TeeSink`, `read_events_from_file`, `fingerprint_workspace`, `default_events_path`, `now_millis`.
- [ ] `kernel-daemon/src/router.rs` still builds and runs — it imports sinks from `kernel_core::session_events`, which re-exports them.
- [ ] All existing tests still pass without modification. Test count unchanged from spec 0008 (78 unit + 15 e2e in kernel-core, 19 in kernel-interfaces).
- [ ] One new test in `kernel-interfaces` confirming the moved types compile and serialize: `session_event_roundtrip_lives_in_interfaces` — serde-round-trips a `SessionEvent::UserInput` using only types from `kernel-interfaces`, no `kernel-core` imports.
- [ ] `kernel-interfaces` does NOT grow any new runtime dependencies. It currently has `serde` and `serde_json` — those are enough for the moved types.
- [ ] No protocol changes.

## Out of scope
- Moving any concrete sink impl. `FileSink` stays in `kernel-core` because it uses `std::fs`, `BufWriter`, etc. — runtime code belongs in runtime crates.
- Moving `NullSink`. It's a stateless drop-all impl but it's still an *impl*, and convention puts impls in core. A distribution that wants a null sink can either depend on `kernel-core` or write the 4-line impl themselves.
- Moving runtime helpers (`read_events_from_file`, `fingerprint_workspace`, `default_events_path`, `now_millis`). All of them touch filesystem, subprocess, env vars, or clock — not interface-crate material.
- Splitting the moved types into more files. One file in interfaces is enough for the current size.
- Adding a new distribution to prove the abstraction works. That's a big spec — this one is just the promotion.
- Backwards compatibility with an old kernel-core-based import path. Since everything inside the workspace gets rebuilt together, rename propagation is mechanical.
- Changing any type signatures. This is a pure move, no API changes.

## Notes

- **Re-export strategy worked perfectly.** Added `pub use kernel_interfaces::session_events::{FingerprintMatch, SessionEvent, SessionEventSink, WorkspaceFingerprint}` at the top of `kernel-core/src/session_events.rs`. Every existing import inside kernel-core (`context.rs`, `event_loop.rs`, `session.rs`) and the daemon (`router.rs`) kept its `kernel_core::session_events::{...}` path and compiled on the first try — zero import rewrites across the workspace. This is the clean way to move types between crates without cascade edits.
- **What moved:** the trait, the enum with all 5 variants, the `WorkspaceFingerprint` + `FingerprintMatch` types (including the `matches()` method), and the forwarding `impl SessionEventSink for Box<dyn SessionEventSink>`. `kernel-interfaces/src/session_events.rs` is now 170 lines of pure abstraction.
- **What stayed:** `NullSink`, `FileSink`, `HttpSink`, `TeeSink`, `fingerprint_workspace`, `read_events_from_file`, `default_events_path`, `now_millis`. All of them touch filesystem, subprocess, env vars, or the clock — runtime material.
- **`kernel-interfaces` dep footprint unchanged.** It already had `serde` and `serde_json` — those are the only deps the moved types need. No new dependencies on the stable API crate.
- **Two new tests in `kernel-interfaces`**: `session_event_roundtrip_lives_in_interfaces` (proves serde works using only interfaces imports) and `workspace_fingerprint_matches_semantics` (a copy of the one that lives in kernel-core's tests, just to verify the matches() method works at the interfaces layer). The `kernel-core` version of the second test is now technically redundant but kept in place — it covers the type as re-exported, which is a marginally different test path.
- **Stable-API graduation criteria met**: the abstraction is now on the side of the boundary that distributions can depend on without pulling in `kernel-core`. A future distribution can `impl SessionEventSink for MyRedisSink { ... }` with only `kernel-interfaces` in its `Cargo.toml`.
- **Verify loop**: `cargo fmt -- --check && cargo clippy && cargo test` all green. kernel-interfaces 21 unit (was 19, +2). kernel-core, dist-code-agent, kernel-daemon test counts unchanged.
- **Judge pass skipped** per 0004 Notes rationale.
