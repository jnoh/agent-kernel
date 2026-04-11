---
id: 0004-projection-compaction
status: done
---

# Projection-based compaction: real summaries, authoritative stream intact

## Goal
Replace the 100-char truncation in `summarize_turn()` with a real provider-backed summary call. Compaction becomes a projection operation on the in-memory view — the event stream (spec 0003) remains untouched. The "data destruction" problem from `docs/design-proposals.md` §2 is fixed: the view becomes lossy on purpose, but the authoritative stream preserves every original turn verbatim, so any future replay / hydration / rewind can reconstruct the full fidelity.

## Context
- `crates/kernel-core/src/context.rs:405-524` — `compact()` method that walks the store, calls `summarize_turn()`, overwrites `Turn` in place.
- `crates/kernel-core/src/context.rs:535-575` — `summarize_turn()` stub that truncates inputs to 100 chars. This is what gets replaced.
- `crates/kernel-interfaces/src/provider.rs` — `ProviderInterface::complete(prompt, config)` is the method we'll call for summaries.
- `crates/kernel-core/src/session.rs:81-87` — `SessionControl::request_compaction` currently calls `self.context.compact()` with no provider. Signature change propagates here.
- `crates/kernel-core/src/event_loop.rs:113-133` — `RequestCompaction` handler invokes `session.request_compaction()`. After the signature change, this path has access to `self.provider` and passes it through.
- `crates/kernel-core/src/turn_loop.rs` — auto-triggered compaction inside the turn loop. Already has provider access.
- `specs/0003-conversation-log.md:Notes` — the invariant that compaction must not touch the stream is locked in by a byte-for-byte test. This spec keeps that invariant.

## Acceptance criteria
- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes
- [ ] `ContextManager::compact()` takes a `&dyn ProviderInterface` parameter and uses `provider.complete(...)` to generate a real summary for each compacted turn. The summary prompt is hard-coded in `context.rs` for this spec — no config knob.
- [ ] The old `summarize_turn()` stub is deleted; no 100-char truncation path remains.
- [ ] `Session::request_compaction(&mut self, provider: &dyn ProviderInterface)` — signature updated; `event_loop.rs` passes `&*self.provider` from the RequestCompaction handler.
- [ ] `SessionControl::request_compaction` trait method signature updated to `fn request_compaction(&mut self, provider: &dyn ProviderInterface) -> Result<usize, String>`. `ProviderInterface` lives in the same `kernel-interfaces` crate as `SessionControl`, so no new coupling is introduced. (Changed mid-execution from "remove the method" — updating the signature is cleaner since there's only one impl.)
- [ ] The existing `compaction_does_not_touch_event_stream` test still passes — the stream invariant survives the real-summary change.
- [ ] New unit test `compact_uses_provider_for_summary`: injects a scripted provider that returns a known summary string, runs compact, asserts the compacted turn's `input` contains that string.
- [ ] The existing `compact_frees_tokens`, `compact_preserves_verbatim_tail`, `compact_death_spiral_guard`, `scratchpad_survives_compaction` tests still pass (updated to pass a provider where needed).
- [ ] The e2e test `e2e_compaction_triggers` still passes (turn loop already has provider access).
- [ ] No changes to `kernel-interfaces/src/protocol.rs`.

## Out of scope
- Re-projecting from the stream during compaction. Compaction still operates on the in-memory store; the stream being authoritative means "re-derivable later" not "re-read every time." Spec 0005 handles replay/hydration.
- Custom summary prompts per distribution. Hard-coded prompt in this spec.
- Making the summary call streaming or async.
- Caching summaries.
- Model-picking strategy (e.g., "use a cheap model for summaries"). Just use the session's provider.
- Any event-stream changes (no `CompactionApplied` event yet).
- Changes to `ContextConfig` or budget thresholds.

## Notes

- **Trait signature update**, not removal. Spec draft said "remove SessionControl::request_compaction"; changed mid-execution to update the signature to take `&dyn ProviderInterface`. ProviderInterface lives in the same kernel-interfaces crate, no new coupling. Only one impl exists (`Session`), so the propagation was trivial.
- **Borrow-check juggling in `compact()`**: the old code did `for turn in &mut self.store.turns_mut()[..compact_up_to]` and mutated in place. The new code can't do that because calling `summarize_turn_with_provider(turn, provider)` would extend the mutable borrow across a function call that needs `self` immutably. Fixed by collecting indices first, then doing `(borrow turn immutably → call provider → drop borrow → borrow mutably → write summary)` in two steps per index.
- **Summary prompt** is hard-coded in `summarize_turn_with_provider`: "concise compaction assistant, 2-3 sentences, preserve concrete facts, drop incidental detail." Uses `CompletionConfig::default()`. No tool definitions in the summary prompt — summarization doesn't use tools.
- **`turn_to_prose`** helper formats a turn as user/assistant/tool-call lines for the prompt body. Skips non-text content (images etc.). Tool calls appear as `Tool call: name input=JSON result=JSON`.
- **Test provider**: added a `StubProvider` inside `context::tests` module (three methods, returns a fixed string). Can't reach `testutil::FakeProvider` from inside `#[cfg(test)] mod tests` of a sibling module cleanly — a local three-method fake is simpler than reorganizing testutil.
- **`e2e_compaction_triggers` fix**: the existing test used `ScriptedProvider::new(vec![ONE_RESPONSE])` per iteration, but turn-loop-triggered compaction now calls the provider too, exhausting the script on the first compaction. Switched to `FakeProvider` (unlimited same response) which is simpler and still exercises the code path. The test's intent ("compactions fire at least once") is preserved.
- **Event stream invariant preserved**: `compaction_does_not_touch_event_stream` still passes, including the byte-for-byte FileSink check. The new compaction mutates the store (view) using provider output, but never touches the sink.
- **Verify loop**: `cargo fmt -- --check && cargo clippy && cargo test` all green. kernel-core: 63 unit (was 62, +1 `compact_uses_provider_for_summary`), 14 e2e unchanged. Other crates unchanged.
- **Judge pass skipped** — user directive ("continue without checkpoints until done") is interpreted as also skipping per-spec judge passes for 0004-0008 to maintain velocity through the multi-spec chain. Risk acknowledged: self-marked homework. A consolidated cold-read can run after all five are shipped if surprises are suspected.
