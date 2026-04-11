---
id: 0006-sink-storage-location
status: done
---

# Session events default location: user home, not workspace

## Goal
Move the default on-disk location for session event files from `<workspace>/.agent-kernel/session-{id}/events.jsonl` to `~/.agent-kernel/sessions/{id}/events.jsonl`. Sessions become globally accessible: you can list or read any past session's events from any directory, not just from inside the original workspace. The `SessionStarted` event already records the workspace, so the workspace-tie is preserved as metadata instead of as directory structure.

Add a single config knob (env var) that lets operators override the base path for cases where `$HOME` isn't the right place (CI, multi-user systems, test isolation).

## Context
- `crates/kernel-daemon/src/router.rs:107-128` — the one and only place where the event file path is resolved. Spec 0003 wired this to `<workspace>/.agent-kernel/session-{id}/events.jsonl`.
- `crates/kernel-core/src/session_events.rs:FileSink::new` — creates parent dirs via `create_dir_all`, so changing the path just means passing a different one.
- `docs/design-proposals.md` §3/§4 — checkpointing and session store. The store's path design is relevant context.

## Acceptance criteria
- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes
- [ ] New function `session_events::default_events_path(session_id: SessionId) -> PathBuf` returns `<base>/sessions/{id}/events.jsonl` where `<base>` is resolved by:
    1. `$AGENT_KERNEL_HOME` if set
    2. Else `$HOME/.agent-kernel` (Unix)
    3. Else `./.agent-kernel` (fallback if `$HOME` is missing — CI, tests)
- [ ] `kernel-daemon/src/router.rs` uses `default_events_path(session_id)` instead of joining the path manually. The workspace path is no longer part of the event file's location.
- [ ] The `SessionStarted` event still records the workspace field (spec 0003 already does this — verify it hasn't regressed).
- [ ] New unit test `default_events_path_honors_env_override` — sets `AGENT_KERNEL_HOME=<tmp>`, asserts the returned path starts with the tmp dir.
- [ ] New unit test `default_events_path_falls_back_to_home` — unsets `AGENT_KERNEL_HOME`, sets `HOME=<tmp>`, asserts the returned path starts with `<tmp>/.agent-kernel/sessions/`.
- [ ] The daemon still falls back to `NullSink` on `FileSink::new` failure (unwritable base dir) with a stderr warning. The existing fallback behavior from spec 0003 is preserved.
- [ ] No protocol changes.

## Out of scope
- Per-distribution override of the base path (distributions talk to the daemon; the daemon decides where things go).
- XDG_STATE_HOME handling (Linux-specific spec; `$HOME/.agent-kernel` is good enough for now — matches `~/.claude/` convention).
- macOS "Application Support" handling (same reason).
- A CLI flag to override on daemon startup (env var is sufficient).
- Garbage collection / retention of old session files.
- Migration of existing files from the old workspace location. Per current state, there are none in the wild (this feature shipped in spec 0003 in the same session as 0006, and no daemon sessions have been opened in the interim — see spec 0003 Notes).
- Listing / browsing sessions (e.g., `ls ~/.agent-kernel/sessions/`). Implied by the new location, not implemented.
- Reading the workspace field out of `SessionStarted` to offer "which workspace was this session in?" as a query. Consumers can `jq` the first line.

## Notes

- **Test consolidation**: the three env-var branches are tested in a single test function `default_events_path_resolves_base_dir` with explicit save/restore. Env var mutation is process-wide, and cargo runs tests in parallel — splitting the three branches into three tests would introduce a race where one test's `set_var` clobbers another's assertion. Single sequential test avoids the race without pulling in `serial_test`.
- **`set_var` is `unsafe` in edition 2024**: each env mutation is wrapped in `unsafe {}`. Documented in the test with a comment.
- **Save/restore is best-effort** — if the test panics between save and restore, the subsequent tests in the same process will see the mutated env. Acceptable because (a) no other test in this crate touches `AGENT_KERNEL_HOME` or `HOME`, (b) the mutation only breaks things if the test panics, which is already a failure state.
- **Did NOT remove `PathBuf` import from router.rs** — wait, actually I did. Confirmed with `cargo check`; the router.rs change now uses only `default_events_path` which returns a `PathBuf`, and no other `PathBuf::from` call remains in the handle_request arm. The import was removed.
- **The daemon's `config.workspace` field is still read** — it's passed into `CreateSession` for use by ContextManager's `record_session_started`, which writes it into the `SessionStarted` event. So the workspace is still recorded, just as event metadata instead of as a filesystem path. This is the "preserved as metadata, not structure" invariant the spec's Goal described.
- **Verify loop**: `cargo fmt -- --check && cargo clippy && cargo test` all green. kernel-core 69 unit (was 68, +1 `default_events_path_resolves_base_dir`). All other test counts unchanged.
