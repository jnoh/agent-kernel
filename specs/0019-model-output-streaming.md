---
id: 0019-model-output-streaming
status: draft
---

# Model output streaming

## Goal

Stream model text output token-by-token to the TUI while the response
is still generating. Currently the user sees nothing for 5-30 seconds
while the Anthropic API call blocks, then a wall of text appears. After
this spec, text appears incrementally as the model produces it.

## Context

- `crates/kernel-interfaces/src/provider.rs` — `ProviderInterface` has
  only `complete()` (blocking). No streaming variant. `ProviderCaps` has
  `supports_streaming: bool` but nothing backs it.
- `crates/kernel-interfaces/src/types.rs:140-148` — `StreamChunk` enum
  already defined: `Text`, `ToolCallStart`, `ToolCallDelta`,
  `ToolCallEnd`, `Done`. Ready to use.
- `crates/kernel-interfaces/src/frontend.rs:36-37` —
  `on_stream_chunk(&self, chunk: &StreamChunk)` exists on
  `FrontendEvents` but is never called.
- `crates/kernel-core/src/proxy_frontend.rs:71-74` —
  `on_stream_chunk` is a no-op with a comment saying "will be added
  when the provider supports streaming."
- `crates/kernel-core/src/turn_loop.rs:183-185` — calls
  `provider.complete()` blocking. No conditional streaming path.
- `crates/kernel-providers/src/anthropic.rs` — uses `ureq::post` with
  no `stream: true`. Reads full response body. `supports_streaming:
  false`.
- Anthropic streaming API: `stream: true` in request body, response is
  SSE with event types `message_start`, `content_block_start`,
  `content_block_delta`, `content_block_stop`, `message_delta`,
  `message_stop`. Each delta carries either `text_delta` or
  `input_json_delta`.

## Design decisions (locked)

**New provider method.** Add `complete_stream` to `ProviderInterface`
with a default that falls back to `complete` + synthetic chunks. This
avoids breaking `EchoProvider` or any future provider that doesn't
support streaming. The turn loop checks `provider.capabilities()
.supports_streaming` and calls the streaming variant when available.

**SSE parsing in AnthropicProvider.** Use `ureq`'s response body as a
`Read` stream, parse `data: {...}` lines. No dependency on an SSE
crate — the format is simple enough for manual parsing (line-buffered
read, skip lines not starting with `data: `, parse JSON).

**Turn loop assembles response from chunks.** The streaming path
accumulates text + tool call deltas into a full `Response` as chunks
arrive, forwarding each `StreamChunk` to `on_stream_chunk`. At stream
end, the assembled `Response` feeds into the same tool-dispatch path
the blocking call uses. No parallel code paths for tool handling.

**KernelEvent::ModelStreamChunk.** New wire event so the TUI can render
incremental text. `ProxyFrontend::on_stream_chunk` sends it over the
crossbeam channel.

**TUI renders streaming text.** `AssistantText` entries grow
incrementally as chunks arrive. The TUI already re-renders on
`app.dirty = true` every 50ms poll cycle, so appending to the last
`AssistantText` entry and marking dirty is sufficient.

## Acceptance criteria

- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes.
- [ ] `ProviderInterface` gains `complete_stream` with default impl.
- [ ] `AnthropicProvider` sends `stream: true`, parses SSE, emits
      `StreamChunk` variants. `supports_streaming` becomes `true`.
- [ ] Turn loop calls `complete_stream` when `supports_streaming` is
      true; assembles full `Response` from chunks for tool dispatch.
- [ ] `KernelEvent::ModelStreamChunk` added to protocol.
- [ ] `ProxyFrontend::on_stream_chunk` sends `ModelStreamChunk`.
- [ ] TUI appends streaming text to the conversation pane live.
- [ ] Tool calls still work correctly when streamed (deltas assembled
      into complete `Content::ToolCall` before dispatch).
- [ ] EchoProvider continues to work (uses default `complete_stream`
      fallback).

## Out of scope

- Streaming tool output into the model context (model still sees one
  complete `tool_result` at call end).
- Streaming cancellation (Ctrl+C during streaming). Existing cancel
  path works at turn boundaries; mid-stream cancel is a future spec.
- Token-level timing or throughput metrics.

## Checkpoints

Standing directive: skip checkpoints, execute to completion.

## Notes

Empty at draft time.
