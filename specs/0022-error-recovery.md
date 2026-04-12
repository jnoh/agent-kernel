---
id: 0022-error-recovery
status: draft
---

# Error recovery and retry logic

## Goal

Stop treating all provider errors as fatal. Currently a 429 rate limit,
a network hiccup, or a compaction failure kills the entire session with
`recoverable: false`. After this spec, transient errors are retried
with exponential backoff, permanent errors are reported without killing
the session, and the user can continue after most failures.

## Context

- `crates/kernel-providers/src/anthropic.rs:100-134` — `complete()` has
  no retry logic. 429 returns `RateLimited { retry_after_secs: None }`.
  No request timeout configured.
- `crates/kernel-core/src/turn_loop.rs:183-185` — any provider error
  becomes `TurnError::Provider`.
- `crates/kernel-core/src/event_loop.rs:215-223` — all `TurnError`s
  are sent as `KernelEvent::Error { recoverable: false }` and break
  the event loop. Session is dead.
- `crates/kernel-core/src/context.rs:533-561` — compaction has cooldown
  and failure-count guards, but failures still propagate as errors.
- `crates/kernel-interfaces/src/provider.rs:50-63` — `ProviderError`
  enum: `RateLimited`, `Api { status, message }`, `Network`, `Parse`,
  `ContextTooLong`.

## Design decisions (locked)

**Classify errors into transient vs permanent.** The retry decision
is based on error type, not caller context:

- **Transient (retry):** `RateLimited`, `Network`, `Api { status: 500..599 }`.
- **Permanent (report, don't retry):** `Parse`, `ContextTooLong`,
  `Api { status: 400..499 except 429 }`.

**Retry in the provider, not the turn loop.** `AnthropicProvider
::complete()` handles retries internally — the turn loop sees either
a successful response or a permanent failure. This keeps retry policy
(backoff timing, max attempts) local to the provider and out of the
core runtime.

**Exponential backoff with jitter.** Starting at 1s, doubling each
attempt, capped at 30s, with random jitter of +/- 25%. Max 3 retry
attempts for transient errors. For 429 specifically, use the
`retry-after` header value if present.

**HTTP request timeout.** Set a 60-second timeout on `ureq` requests
so a hung server doesn't block the session forever.

**Compaction failures are recoverable.** Change `event_loop.rs` to
send compaction errors as `recoverable: true` instead of breaking the
loop. The session continues with an uncompacted context. The user sees
an error message but can keep working. Auto-compaction retries on the
next turn that exceeds the threshold.

**Provider errors in the turn loop are recoverable.** Change
`event_loop.rs` to send provider errors as `recoverable: true` and
NOT break the loop. The user sees the error and can retry by sending
another message. Only `ContextTooLong` after failed compaction is
session-ending.

**TUI shows retriable errors differently.** Transient errors that
were retried and eventually failed show the attempt count:
`"API error after 3 retries: rate limited"`. The user knows the system
tried.

## Acceptance criteria

- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes.

### Provider retry logic

- [ ] `AnthropicProvider::complete()` retries transient errors up to 3
      times with exponential backoff (1s, 2s, 4s base, jittered).
- [ ] 429 responses use `retry-after` header when present.
- [ ] `ureq` requests have a 60-second timeout.
- [ ] Permanent errors (400, 401, 403, parse errors) return immediately
      without retry.
- [ ] Unit test: mock a sequence of 429 → 429 → 200 and verify the
      third attempt succeeds.
- [ ] Unit test: mock a 401 and verify no retry.

### Event loop recovery

- [ ] Provider errors in `event_loop::run_until_yield` are sent as
      `recoverable: true` and do NOT break the loop.
- [ ] Compaction errors are sent as `recoverable: true` and do NOT
      break the loop.
- [ ] After a recoverable error, the next `AddInput` starts a new turn
      normally.
- [ ] `ContextTooLong` after a failed compaction attempt is sent as
      `recoverable: false` (the session truly cannot continue).

### TUI

- [ ] Recoverable errors display in the conversation pane but do not
      stop the spinner or disable input.
- [ ] Error messages include retry context when applicable.

## Out of scope

- Provider fallback (switching to EchoProvider on failure).
- Request queuing or rate-limit-aware scheduling.
- Circuit breaker pattern.
- Streaming error recovery (that's spec 0019 territory).
- Retry for MCP subprocess failures (spec 0016 already handles one
  respawn attempt).

## Checkpoints

Standing directive: skip checkpoints, execute to completion.

## Notes

Empty at draft time.
