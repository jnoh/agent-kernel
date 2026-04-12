---
id: 0016-mcp-stdio-transport
status: draft
---

# MCP stdio transport + streaming chunks (the transport half)

## Goal
Introduce a subprocess-based toolset transport that speaks JSON-RPC 2.0
over child stdio in the shape of Anthropic's Model Context Protocol (MCP).
Every first-party toolset — including the local workspace — becomes a
separate process spawned by the daemon at startup. The daemon's factory
registry gains a new kind, `mcp.stdio`, whose factory function spawns the
declared `command` with `args`, runs `initialize` + `tools/list` to
discover what it advertises, and hands back an `McpStdioToolSet: ToolSet`.
`kernel-workspace-local` grows a binary entry point alongside its library
that IS an MCP server — `main()` is a blocking stdio JSON-RPC loop that
dispatches on method name and reuses the six tool structs the library
already exposes.

Streaming: the shell tool's server-side impl emits stdout/stderr line by
line via `notifications/progress` messages with an `agent_kernel/chunk`
extension field. The client-side `McpToolHandle::execute` interleaves
those notifications with the final `tools/call` response; each chunk is
forwarded through `ToolExecutionCtx::emit_chunk` to the kernel's
`FrontendEvents::on_tool_output_chunk`. A new `KernelEvent::ToolOutputChunk`
crosses the socket so the TUI can render live shell output. The model
still sees a single complete `tool_result` at call end — the buffered
concatenation of every chunk emitted during execute, or the response's
own `content` block if the server returned one.

After 0016, `distros/code-agent.toml` points at `kind = "mcp.stdio"`,
`command = "kernel-workspace-local"`, and the in-process
`kind = "workspace.local"` factory is deleted from the default registry.
Adding a third-party MCP server (GitHub, Postgres, Slack) becomes "ship
or install the binary, add a `[[toolset]]` entry." Zero kernel changes.

## Context
- `crates/kernel-workspace-local/Cargo.toml` — add a `[[bin]]` entry
  named `kernel-workspace-local` pointing at a new `src/main.rs`. The
  existing library stays as the tool impls.
- `crates/kernel-workspace-local/src/main.rs` — new file. Blocking stdio
  JSON-RPC loop: read a line → parse as a JSON-RPC request → dispatch on
  method (`initialize`, `tools/list`, `tools/call`) → write the response
  JSON to stdout. Uses the existing `FileReadTool`/etc. from the lib.
  `ShellTool` runs differently here: spawns the child command with
  stdout/stderr piped and streams each line as a
  `notifications/progress` message. Parses `root` (defaults to `.`) from
  the `initialize` params.
- `crates/kernel-workspace-local/src/lib.rs` — small change: `ShellTool`
  gains a `run_streaming` method that takes a chunk emitter closure and
  spawns/reads the child command line by line. The existing non-streaming
  `execute` impl stays for in-process callers (tests) but is no longer
  the path taken by the daemon.
- `crates/kernel-core/src/mcp_stdio.rs` — new module. Hosts
  `McpStdioToolSet` + `McpToolHandle`. The toolset owns the child
  process, the reader/writer pipe halves, and a cache of the tools
  advertised during `tools/list`. Each `McpToolHandle` holds an `Arc`
  to a shared `Mutex`-guarded client so `execute` can serialize
  `tools/call` requests across concurrent (but unlikely v0.2) callers.
- `crates/kernel-core/src/lib.rs` — `pub mod mcp_stdio;`.
- `crates/kernel-daemon/src/toolset_pool.rs` — `default_registry`
  changes: `"workspace.local"` entry removed, `"mcp.stdio"` entry added
  pointing at `kernel_core::mcp_stdio::from_entry`. `kernel-daemon`
  loses its `kernel-workspace-local` dep — the binary is invoked by
  path, not linked.
- `crates/kernel-daemon/Cargo.toml` — remove the `kernel-workspace-local`
  dep. Add nothing else (the new MCP client code lives in `kernel-core`
  which the daemon already depends on).
- `crates/dist-code-agent/Cargo.toml` — keep the `kernel-workspace-local`
  dep for `TOOL_NAMES` only. This is a deliberate coupling for prompt
  rendering; spec 0017+ will replace it with a protocol query.
- `crates/kernel-interfaces/src/protocol.rs` — add
  `KernelEvent::ToolOutputChunk { session_id, tool_name, stream, data }`.
  This is the first wire-level chunk event and carries
  `ToolChunkStream` directly (round-tripped via serde).
- `crates/kernel-core/src/proxy_frontend.rs` — override
  `on_tool_output_chunk` to send `ToolOutputChunk` over the wire. The
  previous empty default in 0015 is replaced with a real impl.
- `crates/dist-code-agent/src/main.rs` — add a `ToolOutputChunk` branch
  to the TUI event handler. For now, render as appended text under the
  in-progress `ToolCall` entry (or accumulate in the `result_summary`
  field while status is `Running`). A dedicated streaming UI is spec
  0017+ territory — 0016 just needs the chunks to be visible somehow.
- `distros/code-agent.toml` — replace `kind = "workspace.local"` with
  `kind = "mcp.stdio"`, `command = "kernel-workspace-local"`,
  `args = []`, `[toolset.config]` untouched.

## Design decisions (locked)

**MCP subset.** We implement only what's needed:
  - `initialize` — request, with `params: { protocolVersion, clientInfo,
    capabilities, agent_kernel: {...} }`. The `agent_kernel` key under
    params is our extension point for passing `root` etc. without
    colliding with real MCP servers. Server response includes
    `serverInfo` + `capabilities` (which we parse but only log).
  - `tools/list` — request. Response includes `tools: [{ name,
    description, inputSchema }]`. We build `ToolRegistration` wrappers
    from the response.
  - `tools/call` — request with `params: { name, arguments,
    _meta: { progressToken } }`. Response content is an array of
    content items; we concatenate text items into the final result
    the model sees. If `content` is empty AND we received chunks,
    we use the concatenated chunk buffer as the result.
  - `notifications/progress` — server-to-client notification. Carries
    `params: { progressToken, progress, total, message,
    _meta: { agent_kernel/chunk: { stream, data } } }`. Standard MCP
    servers ignore the `_meta` field; ours reads it.

We DO NOT implement resources, prompts, sampling, roots, logging, or
any other MCP capability in 0016. Third-party MCP servers that advertise
those in `initialize` capabilities are logged and ignored.

**Wire format.** Each JSON-RPC message is newline-delimited (JSON-RPC
2.0 over stdio uses Content-Length headers in the MCP spec, but our
first-party server controls both ends so we can use newline-delimited
JSON for simplicity). A follow-up spec can upgrade to proper
Content-Length framing when we start talking to real third-party servers.
Note this in the spec Notes and in the architecture doc.

**Process lifecycle.** Per-daemon. `ToolsetPool::build` spawns every
child once at daemon startup. Children live for the daemon's lifetime.
If a child's stdout pipe EOFs (child crashed), the owning
`McpStdioToolSet` flips a `dead: AtomicBool` flag. The next `tools/call`
on any of its tools attempts one synchronous respawn via a stored copy
of the original `ToolsetEntry` + `initialize` params; on success the
call proceeds, on failure it returns `ToolError::Transport(...)`.
No backoff, no retries, no health checks.

**Concurrency model.** `McpStdioToolSet` owns a child process, the
reader half of its stdout pipe, and the writer half of its stdin
pipe. Calls are serialized through a `Mutex` on an inner client — even
though the turn loop is single-threaded per session, multiple sessions
sharing one pool will serialize `tools/call`s on the same child. This
is fine for v0.2; real concurrency is a later spec concern.

The reader side is NOT a background thread. `execute` is blocking: it
writes the `tools/call` request, then reads messages in a loop until
it sees either its own response (matched by JSON-RPC id) or
`notifications/progress` with its progressToken (which it forwards to
the ctx). Unrelated messages — progress for other calls, unknown
notifications — are logged and dropped.

**Progress token.** A monotonic `u64` counter inside the client, wrapped
as a string (`"p{n}"`) for JSON-RPC compatibility. Each `tools/call`
allocates a fresh token, attaches it to `params._meta.progressToken`,
and uses it to filter incoming progress messages.

**Tool schemas on the client side.** After `tools/list` returns, we
cache `ToolSchema`-shaped entries (`name`, `description`, `inputSchema`,
etc.) keyed by name. Each `McpToolHandle` holds a reference to its
cached schema (via `Arc<ToolSchemaCache>`) and a shared `Arc<Mutex<McpClient>>`
back to the toolset. Capabilities are synthesized — MCP doesn't advertise
capabilities in `tools/list`, so we infer: anything with "read", "get",
"list" in the name gets `fs:read`; writes/edits get `fs:write`; shell gets
`shell:exec`. This is brittle but it works for our one first-party server.
0017+ will extend the MCP wire format with a capabilities-in-schema
convention or similar.

**`KernelEvent::ToolOutputChunk`.** New wire-level event:

```rust
KernelEvent::ToolOutputChunk {
    session_id: SessionId,
    tool_name: String,
    stream: ToolChunkStream,
    data: String,
}
```

Flows: subprocess → `notifications/progress` → `McpToolHandle::execute`
reader loop → `ctx.emit_chunk(...)` → `FrontendEvents::on_tool_output_chunk`
→ `ProxyFrontend` sends `ToolOutputChunk` over the socket → TUI renders.

**Deleted in 0016.** Nothing new deleted in kernel crates. The
`workspace.local` kind registration disappears from `default_registry`;
`kernel_workspace_local::from_entry` becomes dead code but stays in the
library for now in case anyone wants in-process testing. The dist's
dependency on `kernel-workspace-local` stays for `TOOL_NAMES`.

## Acceptance criteria

- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes.

### kernel-interfaces

- [ ] `KernelEvent::ToolOutputChunk { session_id, tool_name, stream, data }`
      added to `protocol.rs`. Round-trip test covers it.
- [ ] `ToolChunkStream` already derives `Serialize + Deserialize` from
      spec 0015; verify and adjust if needed.

### kernel-workspace-local

- [ ] `Cargo.toml` adds `[[bin]] name = "kernel-workspace-local"` path
      `src/main.rs`. Keeps existing `[lib]`.
- [ ] `src/main.rs` — blocking stdio JSON-RPC loop. Implements
      `initialize`, `tools/list`, `tools/call`. Reads `root` from
      `params.agent_kernel.root` (default `.`), constructs a
      `LocalWorkspace`, holds it for the process lifetime.
- [ ] `tools/call` for shell uses a new streaming path — spawns
      `sh -c <cmd>` with piped stdout/stderr, reads line by line,
      emits each line as a `notifications/progress` message with
      the `agent_kernel/chunk` extension. Final response carries
      `exit_code`, concatenated `stdout`, and `stderr`.
- [ ] `tools/call` for the other five tools uses `ToolExecutionCtx::null()`
      and the existing in-process `execute` (no chunks).
- [ ] Integration test in `tests/` that spawns the binary, sends
      `initialize`, `tools/list`, and a non-streaming `file_read` call,
      and asserts the responses are well-formed. (Skips the streaming
      path — that's covered by the `kernel-core::mcp_stdio` tests.)

### kernel-core

- [ ] New module `src/mcp_stdio.rs` with `McpClient`, `McpStdioToolSet`,
      `McpToolHandle`, and `from_entry(entry: &ToolsetEntry) ->
      Result<Box<dyn ToolSet>, String>`.
- [ ] `McpClient` owns the child, its stdin writer, its stdout
      line-buffered reader, and a next-id counter.
- [ ] `McpStdioToolSet::from_entry` spawns the child per
      `entry.config.command` (string) + `entry.config.args` (array),
      runs `initialize` (forwarding `entry.config` minus `command`/`args`
      under `params.agent_kernel`), runs `tools/list`, builds an
      `Arc<ToolsetCache>` with the advertised schemas, returns the
      boxed toolset.
- [ ] `impl ToolSet for McpStdioToolSet` — `tools()` returns
      `McpToolHandle` instances, each holding an `Arc<McpClient inside Mutex>`
      and a reference to its cached schema.
- [ ] `impl ToolRegistration for McpToolHandle` — `execute` allocates
      a progress token, sends `tools/call`, reads messages in a loop
      forwarding `notifications/progress` to `ctx.emit_chunk`,
      terminates on the matching response. Buffers chunks for the final
      `tool_result` (used when response `content` is empty).
- [ ] Crash handling: stdout EOF during a call → mark client dead →
      return `ToolError::Transport`. Next call attempts one synchronous
      respawn via the stored entry.
- [ ] Unit tests using a tiny mock server binary target OR a direct
      in-memory `McpClient::from_pipes` constructor. Cover: initialize
      handshake, non-streaming call, streaming call with chunks, crash
      recovery.

### kernel-daemon

- [ ] `toolset_pool::default_registry` removes `"workspace.local"`, adds
      `"mcp.stdio" → kernel_core::mcp_stdio::from_entry`.
- [ ] `Cargo.toml` removes the `kernel-workspace-local` dep.
- [ ] Existing tests in `toolset_pool::tests` that reference
      `workspace.local` update to use `mcp.stdio` pointing at a mock
      command, OR switch to an in-tree dummy registry.

### Wire-event plumbing

- [ ] `ProxyFrontend::on_tool_output_chunk` overrides the default,
      sends `KernelEvent::ToolOutputChunk` with the session id.
- [ ] `dist-code-agent::main` TUI event handler handles
      `ToolOutputChunk`: locate the most recent in-progress `ToolCall`
      entry for that tool name and append `data` to a chunk-log field
      (new on `ConversationEntry::ToolCall`, or reuse
      `result_summary` with a "(streaming)" prefix). Minimal viable
      rendering; polish is out of scope.
- [ ] REPL event handler just `eprintln!`s chunks with a `[tool] ...`
      prefix.

### distros/code-agent.toml

- [ ] Entry becomes:
  ```toml
  [[toolset]]
  kind = "mcp.stdio"
  id = "workspace"
  [toolset.config]
  command = "kernel-workspace-local"
  args = []
  root = "."
  ```
- [ ] Document how the binary is found: for dev, via `cargo run` the
      binary is at `target/debug/kernel-workspace-local`. The daemon
      spawns `command` using `std::process::Command::new`, which
      searches `$PATH`. Document the two-part process for running
      locally in the spec Notes (either `cargo install --path
      crates/kernel-workspace-local` or prepend `target/debug` to `$PATH`).

### docs

- [ ] doc-sync subagent covers architecture.md and design-proposals.md.

## Out of scope

- **Content-Length framing.** Newline-delimited JSON-RPC in 0016; MCP
  spec's Content-Length header framing lands in a later spec before we
  claim wire compatibility with real MCP servers.
- **Full MCP capability set.** No resources, prompts, sampling, roots,
  logging, or cancellation. Only `initialize` / `tools/list` /
  `tools/call` / `notifications/progress`.
- **Third-party MCP server testing.** We don't run any real MCP server
  against our client in 0016. Correctness tests use our own first-party
  server.
- **Streaming into the model context.** Model still sees one complete
  `tool_result` at call end. Live-into-model streaming is not a current
  or planned capability.
- **Per-session toolsets / multi-workspace daemons.** Still one pool
  for the daemon's lifetime.
- **Supervisor refinements.** One synchronous respawn attempt on first
  next call after crash, nothing else.
- **Capability inference from tool names** is a first-draft hack; a
  proper schema-level convention arrives in a later spec.

## Checkpoints

Standing directive: skip checkpoints, execute to completion. Doc-sync
+ verify loop before the final commit.

## Notes

Empty at draft time.
