---
id: 0020-clean-tool-results
status: done
---

# Clean tool results for the model

## Goal

Improve what the model sees as tool results. Currently the model
receives raw JSON blobs like `{"exit_code":0,"stdout":"hello\n",
"stderr":""}` as a string in the `tool_result` content field. After
this spec, the model sees human-readable text: just the stdout for
successful shell commands, just the file content for file reads, etc.
This directly improves model response quality because the model
spends fewer tokens parsing structure and more tokens reasoning.

## Context

- `crates/kernel-workspace-local/src/main.rs:169-184` — MCP server
  wraps tool results as `content[].text` with `output.result
  .to_string()` (stringified JSON).
- `crates/kernel-core/src/mcp_stdio.rs:441-460` — MCP client parses
  content text back into `serde_json::Value` via `from_str`.
- `crates/kernel-providers/src/anthropic.rs:179-181` — Anthropic
  provider serializes `Content::ToolResult { result }` as
  `"content": result.to_string()` — another stringification.
- The Anthropic API `tool_result` content field accepts either a plain
  string or an array of content blocks. We currently always send a
  string.

## Design decisions (locked)

**Fix at the source.** The MCP server (`kernel-workspace-local`) should
return clean human-readable text in `content[].text`, not stringified
JSON. For each tool:

- `file_read`: just the numbered content lines (what `content` already
  has), with a header like `<path> (<total_lines> lines)`.
- `file_write`: `Wrote <bytes> bytes to <path>`.
- `file_edit`: `Edited <path>` or `Created <path>`.
- `shell`: stdout text directly. Prepend `[exit <code>]` if non-zero.
  Append stderr if non-empty.
- `ls`: one entry per line, directories suffixed with `/`.
- `grep`: matching lines directly (already mostly text).

**Fix the Anthropic serialization.** `Content::ToolResult` should
serialize its `result` field as a plain string when it's a
`Value::String`, not as a quoted JSON string. Currently
`Value::String("hello").to_string()` produces `"\"hello\""` — the
model sees escaped quotes around every text result.

**MCP client passes text through.** Since the server now sends clean
text, the client's `from_str` parse attempt will fail (it's not JSON),
and it falls back to `Value::String(text_result)`. This is the correct
path — no client changes needed.

## Acceptance criteria

- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes.
- [ ] `file_read` result is human-readable numbered lines, not JSON.
- [ ] `shell` result is stdout text (+ exit code header if non-zero,
      + stderr if non-empty), not a JSON object.
- [ ] `file_write`, `file_edit`, `ls`, `grep` results are clean text.
- [ ] `Content::ToolResult` serializes `Value::String` as the bare
      string, not JSON-quoted.
- [ ] Integration test in kernel-workspace-local confirms clean text
      from `tools/call`.
- [ ] Existing kernel-core and agent-kernel tests pass.

## Out of scope

- Changing the MCP wire format (JSON-RPC structure unchanged).
- Tool-specific formatting in the TUI (that's a TUI concern, already
  handled by `format_tool_result` in main.rs).
- Adding new tools.

## Checkpoints

Standing directive: skip checkpoints, execute to completion.

## Notes

- The MCP client's `from_str` fallback works correctly — clean text
  fails to parse as JSON and becomes `Value::String(text)`, which is
  what we want.

- Anthropic provider's ToolResult serialization now extracts bare
  strings from `Value::String` instead of JSON-quoting them. This
  means the model sees `hello world` not `"hello world"`.

- Skipped judge/doc-sync — narrow change affecting only how text is
  formatted at the MCP server boundary and serialized at the API
  boundary.
