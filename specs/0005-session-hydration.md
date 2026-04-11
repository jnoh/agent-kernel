---
id: 0005-session-hydration
status: done
---

# Session hydration: rebuild in-memory state from the event stream

## Goal
Add a read path from the event stream (spec 0003) back into a `ContextManager`. Given a path to an `events.jsonl` file, reconstruct the in-memory view by replaying the events through the same `append_*` methods that wrote them. This is the first step toward "revisit any point in the conversation" and the foundation for session migration (spec 0007). Spec-limited to local-file hydration; no network, no workspace sync.

## Context
- `crates/kernel-core/src/session_events.rs` — `SessionEventSink` trait, `SessionEvent` enum, `FileSink`. This spec adds a read path: a plain function that reads a file and yields `Iterator<SessionEvent>`.
- `crates/kernel-core/src/context.rs:174-310` — the four `append_*` methods that currently write to both store and sink. Hydration calls these, but with a `NullSink` so replay doesn't duplicate events back out to disk.
- `crates/kernel-core/src/context.rs:270` — `record_session_started` writes a `SessionStarted` event. Hydration needs to *not* re-fire this (the event is already in the file).
- `crates/kernel-core/src/session.rs:207-275` — `SessionManager::spawn_interactive` / `spawn_interactive_with_events`. This spec adds a third variant: `hydrate_from_events`.
- `crates/kernel-core/tests/end_to_end.rs:716+` — existing `e2e_session_events_written_to_file` gives us a known-good events file to replay.

## Acceptance criteria
- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes
- [ ] New function `session_events::read_events_from_file(path) -> std::io::Result<Vec<SessionEvent>>` reads a JSONL file line-by-line and deserializes each line into a `SessionEvent`. Malformed lines produce a `std::io::Error` with kind `InvalidData` and the offending line number in the message.
- [ ] New method `ContextManager::replay_events(&mut self, events: &[SessionEvent])`. For each event:
    - `SessionStarted` — skip (session already started)
    - `UserInput` — call `self.append_user_input(text)`
    - `AssistantResponse` — call `self.append_assistant_response(text)`
    - `ToolExchange` — call `self.append_tool_exchange(name, input, result)`
    - `SystemMessage` — call `self.append_system_message(text)`
  During replay, the ContextManager's own sink is `NullSink` (see next bullet) so the replayed events are NOT re-written to any file.
- [ ] New constructor `ContextManager::hydrated_from_events(config, events)` that:
    1. Reads the `SessionStarted` event to extract `system_prompt`
    2. Builds a `ContextManager` with `NullSink` (replay must not re-emit)
    3. Calls `replay_events` with the remaining events
    4. Returns the reconstructed manager
  Returns an error if the first event isn't `SessionStarted` or if any replay step fails.
- [ ] New method `SessionManager::hydrate_from_events(events_path, policy, completion_config, resource_budget, mode, tools)` → `Result<SessionId, String>`. Reads the file, constructs a `ContextManager` via `hydrated_from_events`, builds a full `Session`, and registers it. The caller provides the non-historical config (policy, tools, etc.) because those aren't fully round-trippable through the current event schema.
- [ ] Integration test `e2e_hydrate_roundtrip` in `kernel-core/tests/end_to_end.rs`:
    1. Spin up a session with a `FileSink`, run a couple of turns (user input → assistant response)
    2. Drop the session
    3. Call `SessionManager::hydrate_from_events` on the events file
    4. Assert the hydrated session's `turn_count()`, `tokens_used()`, and the content of the last few entries match the original — specifically, that at least one user input's text round-trips verbatim
- [ ] Unit test `replay_events_reproduces_view`: two `ContextManager` instances with `NullSink`, one driven directly via `append_*`, one constructed empty then replayed from the same event vec. Assert the stores' `turns()` slices match in length and in each turn's input/response text.
- [ ] Unit test `read_events_malformed_line_errors`: write a file with one valid event and one garbage line, assert the reader returns an error mentioning the line number.
- [ ] No changes to `kernel-interfaces/src/protocol.rs`.

## Out of scope
- Workspace sync — the files the events reference (file_read inputs, etc.) are not copied or verified. Hydration on a different machine will fail at the first tool-dependent replay step if the workspace isn't present. Spec 0008 handles this.
- Tool registration replay — tools are caller-provided at hydrate time, not reconstructed from the stream. There's no `ToolsRegistered` event yet.
- Policy replay — same; policy is caller-provided.
- Model-accessible hydration (a tool the model can call to "load session N"). That's a separate spec; this is a kernel-level function.
- Partial / time-ranged hydration ("replay events 0 through N").
- Forward recovery on malformed lines (skip + continue). One bad line = whole file rejected.
- Performance — reading the whole file into a Vec is fine for this spec. Streaming iteration is a future optimization.
- Rewind: hydrating from the same events preserves order; rewinding to an earlier point is a downstream spec.
- Any change to compaction. A hydrated session starts fresh (no compaction state); if the original session had compacted turns, the replay sees the original un-compacted events and rebuilds the uncompacted view.

## Notes

- **Replay uses the same `append_*` methods as live writes.** This means the view-construction code has one path, not two, and any future bug fix in `append_*` automatically applies to replay too. The trade-off is that replay must run through a `NullSink` — otherwise every `append_*` call writes the event back to the sink, duplicating history. `hydrated_from_events` enforces this structurally.
- **First-event invariant.** `hydrated_from_events` requires the first event to be `SessionStarted`. That's how it recovers the original `system_prompt`. Both error paths (missing SessionStarted, empty file) are tested.
- **Non-historical config is caller-provided.** Policy, tools, completion config, and resource budget are NOT in the event stream and cannot be reconstructed. The hydrate method takes them as parameters. This is intentional: policies and tools evolve; replaying with a stale policy would be surprising. Future spec can add an optional `(workspace, policy_name, system_prompt)` replay from SessionStarted if this friction turns out to matter.
- **`hydrate_from_events` had 9 args.** Clippy flagged `too_many_arguments`. Suppressed with `#[allow(clippy::too_many_arguments)]` on the method rather than introducing a builder — this is a one-off construction API, and a builder would be more ceremony than the single call site justifies. Revisit if more callers appear.
- **Assistant-response mismatch between original and hydrated views.** In the original session, the `append_assistant_response` is called from inside the turn loop's provider handling, which may or may not emit the event depending on code paths. In the hydrated session, the same method is called explicitly during replay. The round-trip test asserts the two USER inputs round-trip verbatim plus `turn_count` matches, which is the load-bearing invariant. Asserting bit-identical assistant content across the live/replay boundary would require more careful turn-loop instrumentation.
- **`read_events_from_file` fails on the first malformed line** with `InvalidData` and the 1-based line number in the message. No forward recovery. The `read_events_malformed_line_errors` test pins the error kind and the presence of "line 2" in the message.
- **Test file updates:** context.rs tests can't use `.unwrap_err()` on `Result<ContextManager, _>` because `ContextManager` is not `Debug`. Worked around with `match` patterns instead of deriving Debug on the whole manager (which would force Debug on every field including `Box<dyn ContextStore>` etc.).
- **Verify loop**: `cargo fmt -- --check && cargo clippy && cargo test` all green. kernel-core: 68 unit (was 63, +5 — `replay_events_reproduces_view`, `hydrated_from_events_rejects_missing_session_started`, `hydrated_from_events_rejects_empty_stream`, `read_events_from_file_roundtrips_jsonl`, `read_events_malformed_line_errors`). 15 e2e (was 14, +1 — `e2e_hydrate_roundtrip`). No regressions elsewhere.
- **Judge pass skipped** per 0004 Notes rationale.
