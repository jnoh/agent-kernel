---
id: 0015-toolset-trait-and-execution-ctx
status: done
---

# ToolSet trait + ToolExecutionCtx (the load-bearing half)

## Goal
Introduce the `ToolSet` trait, the `ToolExecutionCtx` parameter on
`ToolRegistration::execute`, and the `FrontendEvents` chunk event — and wire
them end-to-end using an **in-process** toolset implementation. Delete
`ProxyTool` and the `RegisterTools`/`ExecuteTool`/`ToolResult` protocol
messages. The six filesystem tools move into a new `kernel-workspace-local`
**library** crate (not a binary) that exposes a `LocalWorkspace` type
implementing `ToolSet`. The daemon constructs it at startup from a
`[[toolset]]` manifest entry with `kind = "workspace.local"` and an opaque
`[toolset.config]` block carrying `root`. Session create snapshots the pool's
tools instead of the frontend's `RegisterTools`.

This spec deliberately does **not** ship the MCP wire protocol, a subprocess
runtime, or streaming semantics. It proves out the trait shape, the execute
context threading, and the factory-registry manifest model. Spec 0016 then
swaps the in-process `LocalWorkspace` transport for an `mcp.stdio` subprocess
transport **without changing any kernel-side trait signatures** — that's the
whole point of doing it in this order.

After 0015, the architecture is "daemon reads `[[toolset]]` entries → dispatches
on `kind` → factory returns a `Box<dyn ToolSet>` → kernel collects tools via
`ToolSet::tools()`." The only thing missing for the full MCP story is the
subprocess transport. 0016 adds it.

## Context
- `crates/kernel-interfaces/src/tool.rs` — current `ToolRegistration` trait
  and `ToolError`/`ToolOutput` types. Gains `ToolExecutionCtx`, `ToolChunk`,
  `ToolChunkStream`, and the new `execute` signature.
- `crates/kernel-interfaces/src/frontend.rs` — `FrontendEvents` trait. Gains
  `on_tool_output_chunk(&self, tool_name, stream, data)`.
- `crates/kernel-interfaces/src/manifest.rs` — gains `ToolsetEntry`. Deletes
  `ToolsConfig` + the `tools: Option<ToolsConfig>` field.
- `crates/kernel-interfaces/src/protocol.rs` — delete `RegisterTools`,
  `ExecuteTool`, `ToolResult`, `ToolSchema`. No chunk event on the wire yet;
  chunk streaming terminates at the daemon's `FrontendEvents` impl. Spec 0016
  widens it to the socket protocol when subprocesses arrive.
- `crates/kernel-core/src/proxy_tool.rs` — deleted outright.
- `crates/kernel-core/src/event_loop.rs` / `src/turn_loop.rs` / `src/session.rs`
  — `execute` callsite threads through a new `ToolExecutionCtx`. The ctx is
  built per-tool-call and wraps a reference to the session's `FrontendEvents`
  impl so chunk emission flows to the frontend.
- `crates/kernel-daemon/src/router.rs` — stop accepting `RegisterTools`.
  Snapshot the pool's tools at session create. Delete `ExecuteTool` /
  `ToolResult` routing. Keep everything else as is.
- `crates/kernel-daemon/src/main.rs` — after the provider factory is built,
  construct a `ToolsetPool` from `manifest.toolsets`. Register the one
  built-in factory: `kind = "workspace.local" → LocalWorkspace::from_config`.
- `crates/kernel-workspace-local` — new **library** crate. Depends on
  `kernel-interfaces` and nothing else in the workspace. Ships the six
  tool impls moved from `dist-code-agent::tools`.
- `crates/dist-code-agent/src/tools.rs` — deleted.
- `crates/dist-code-agent/src/main.rs` — remove the `RegisterTools` send and
  `DistributionSettings::enabled_tools`. `create_tools` goes away.
- `crates/dist-code-agent/tests/tools_test.rs` — deleted; equivalents land in
  `kernel-workspace-local`.
- `distros/code-agent.toml` — replace `[tools]` with `[[toolset]]`.
- `docs/architecture.md` — new subsystem section describing the ToolSet pool.

## Design decisions (locked)

**`ToolSet` trait in `kernel-interfaces::toolset`**:

```rust
pub trait ToolSet: Send + Sync {
    fn id(&self) -> &str;
    fn tools(&self) -> Vec<Box<dyn ToolRegistration>>;
}
```

**`ToolExecutionCtx` is a concrete struct, not a trait.** Holds an optional
chunk-sink callback. Non-streaming tools ignore it:

```rust
pub struct ToolExecutionCtx<'a> {
    chunk_sink: Option<&'a dyn Fn(ToolChunk)>,
}
impl<'a> ToolExecutionCtx<'a> {
    pub fn null() -> ToolExecutionCtx<'static> { ... }
    pub fn with_sink(sink: &'a dyn Fn(ToolChunk)) -> Self { ... }
    pub fn emit_chunk(&self, chunk: ToolChunk) { ... }
}
```

**`ToolChunk` and `ToolChunkStream`** live next to `ToolExecutionCtx`:

```rust
pub struct ToolChunk { pub stream: ToolChunkStream, pub data: String }
pub enum ToolChunkStream { Stdout, Stderr, Text }   // serde-lowercase
```

**`ToolRegistration::execute` signature change (breaking)**:

```rust
fn execute(
    &self,
    input: serde_json::Value,
    ctx: &ToolExecutionCtx<'_>,
) -> Result<ToolOutput, ToolError>;
```

**Manifest**: delete `ToolsConfig`. Add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsetEntry {
    pub kind: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub config: toml::Value,
}
```

`DistributionManifest` gains `#[serde(default)] pub toolsets: Vec<ToolsetEntry>`
and loses `tools`. A legacy `[tools]` section now fails to parse.

**Factory registry in the daemon.** Map of `kind` → factory function:

```rust
type ToolsetFactory = fn(&ToolsetEntry) -> Result<Box<dyn ToolSet>, String>;
```

One registered factory: `"workspace.local" → kernel_workspace_local::from_entry`.

**`ToolsetPool`** lives in `kernel-daemon` (daemon-lifecycle state, not
turn-loop state). Holds `Vec<Box<dyn ToolSet>>` and a merged, collision-checked
cache as `Vec<Arc<dyn ToolRegistration>>`. `tools_for_session()` returns
`Arc`-wrapped tools.

**Session signature change.** `SessionConfig`/`EventLoopConfig` change from
`Vec<Box<dyn ToolRegistration>>` to `Vec<Arc<dyn ToolRegistration>>`. Every
callsite in kernel-core, kernel-daemon, and tests updates accordingly. Arc
is what we want anyway — the pool is shared across sessions.

**Chunk delivery.** `FrontendEvents` gains:

```rust
fn on_tool_output_chunk(&self, tool_name: &str, stream: ToolChunkStream, data: &str) {}
```

with an empty default so existing impls don't all need updating. The turn
loop builds a `ToolExecutionCtx` per tool call whose sink forwards to
`frontend.on_tool_output_chunk`. In 0015 no in-tree tool actually emits
chunks — a unit test with a toy tool exercises the path.

**Protocol surgery.** Delete `KernelRequest::RegisterTools`,
`KernelRequest::ToolResult`, `KernelEvent::ExecuteTool`, `ToolSchema`. No new
variants added. Chunk events cross the socket in 0016 when they need to.

## Acceptance criteria

- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes.

### kernel-interfaces

- [ ] `ToolChunk`, `ToolChunkStream`, `ToolExecutionCtx<'a>` added to
      `tool.rs`. `ToolError::Transport(String)` variant added.
- [ ] `ToolRegistration::execute` takes `ctx: &ToolExecutionCtx<'_>`.
- [ ] New `toolset.rs` module with `ToolSet` trait. Re-exported from crate
      root.
- [ ] `FrontendEvents::on_tool_output_chunk` method with empty default impl.
- [ ] `manifest.rs`: `ToolsetEntry` type added, `DistributionManifest.toolsets`
      field added, `ToolsConfig` + `tools` field deleted.
- [ ] Manifest tests updated: delete old `tools` tests, add
      `parses_manifest_with_toolset_section`,
      `parses_manifest_with_multiple_toolsets`,
      `manifest_with_legacy_tools_section_errors`.
- [ ] `protocol.rs`: delete `RegisterTools`, `ToolResult`, `ExecuteTool`,
      `ToolSchema`. Round-trip tests updated.

### kernel-workspace-local (new library crate)

- [ ] New workspace member at `crates/kernel-workspace-local`, library crate.
- [ ] Depends on `kernel-interfaces` + `serde` + `serde_json` + `toml`.
- [ ] `LocalWorkspace { root, id }`, `impl ToolSet`.
- [ ] `pub fn from_entry(entry: &ToolsetEntry) -> Result<Box<dyn ToolSet>, String>`.
- [ ] Six tool impls moved from `dist-code-agent::tools` with updated execute
      signature.
- [ ] Unit tests mirroring `dist-code-agent/tests/tools_test.rs`.

### kernel-core

- [ ] Delete `crates/kernel-core/src/proxy_tool.rs` and its `pub mod`
      declaration.
- [ ] `event_loop.rs`/`turn_loop.rs`/`session.rs`: every `tool.execute(input)`
      call threads a `ToolExecutionCtx` built per-call. `Session` /
      `SessionConfig` / `EventLoopConfig` tool fields change from `Box` to `Arc`.
- [ ] Chunk-emission unit test: a toy `ToolRegistration` impl calls
      `ctx.emit_chunk` twice, a toy `FrontendEvents` impl counts chunks,
      assertion that both arrived in order.

### kernel-daemon

- [ ] New `toolset_pool.rs` module with `ToolsetPool`, `FactoryRegistry`,
      `default_registry()`. Registry contains `"workspace.local"`.
- [ ] `main.rs`: after `build_provider_factory`, construct
      `ToolsetPool::build(&manifest.toolsets, &default_registry())`. Hard
      startup error on failure.
- [ ] Pool wrapped in `Arc` and passed into `ConnectionRouter::new`.
- [ ] `ConnectionRouter`:
  - [ ] Delete `tool_schemas`, `tool_response_txs` fields.
  - [ ] Delete `RegisterTools` + `ToolResult` handling.
  - [ ] `CreateSession` path calls `pool.tools_for_session()`.
- [ ] Collision test: two entries advertising the same tool name fail at
      `ToolsetPool::build`.

### dist-code-agent

- [ ] Delete `src/tools.rs`, `tests/tools_test.rs`.
- [ ] `main.rs`: remove `RegisterTools` send, `enabled_tools`, `create_tools`
      usage.
- [ ] `Cargo.toml`: drop any deps only used by deleted code.

### distros/code-agent.toml

- [ ] Replace `[tools]` with:
  ```toml
  [[toolset]]
  kind = "workspace.local"
  id = "workspace"
  [toolset.config]
  root = "."
  ```

### docs/architecture.md

- [ ] New subsystem section on the ToolSet pool (via doc-sync subagent).

## Out of scope

- **Subprocess transport / MCP wire protocol.** Spec 0016.
- **In-tree streaming.** The plumbing exists; no in-tree tool emits chunks
  in 0015. A unit test covers the path.
- **Wire-level chunk event.** `KernelEvent::ToolOutputChunk` is NOT added
  here. 0016 adds it when it's load-bearing.
- **Schema-aware policy matching.** Deferred indefinitely.
- **Splitting TUI into its own binary.** Spec 0017+.
- **Supervisor / respawn logic.** No subprocesses in 0015.

## Checkpoints

Standing directive: skip checkpoints, execute to completion. Run the full
verify loop and invoke the doc-sync subagent before the final commit.

## Notes

- **Split from the original 0015.** First draft tried to ship both the
  trait/execute-ctx work AND the MCP subprocess transport + streaming in a
  single spec. Mid-execution realized the scope was ~2500 lines of diff
  with interlocking risk on the `ToolExecutionCtx` threading. Split so
  that the load-bearing architectural change lands first in-tree; spec
  0016 will swap the in-process transport for `mcp.stdio` without touching
  any kernel-side trait signatures.

- **`ToolExecutionCtx` sink is `!Send + !Sync`.** First attempt required
  `Send + Sync` on the `Fn(ToolChunk)` trait object, which failed because
  `dyn FrontendEvents` is only `: Send`. Relaxed the bound — `execute`
  runs synchronously on a single thread, so the ctx lives on that thread
  and the sink is only called from it. If a future toolset transport needs
  to push chunks from a background thread, it should use an internal
  channel and call `emit_chunk` from the execute thread only.

- **`Session` kept `Vec<Box<dyn ToolRegistration>>`**, NOT `Vec<Arc<...>>`.
  The pool calls `ToolSet::tools()` fresh on every session create, which
  returns a fresh owned `Box` per tool. No need to share ownership across
  sessions. Simpler and matches how the trait was actually designed —
  `tools()` is a discovery method, not an accessor.

- **`deny_unknown_fields` on `DistributionManifest`.** Added so a legacy
  `[tools]` section produces a hard parse error instead of being silently
  ignored. Manifests with `[tools]` now fail at load time with a clear
  message naming the unknown field. Tests cover this.

- **Protocol surgery added one variant.** `KernelEvent::ToolCompleted
  { session_id, tool_name, result }` is new. Necessary because the kernel
  now owns tool dispatch — the frontend is no longer running tools itself,
  so it needs a direct notification when a tool completes to render a
  display summary. The old architecture had the distro run the tool
  locally and update the UI in-process; that path is gone. `ProxyFrontend::on_tool_result`
  now forwards through this event instead of being a no-op.

- **Deleted modules**: `kernel-core::proxy_tool`, `kernel-core::in_process`.
  The `in_process` module was deeply coupled to `ProxyTool`/`ToolSchema` and
  its test coverage duplicated `event_loop::tests`. Removed outright.

- **Deleted from `dist-code-agent`**: `src/tools.rs`, `tests/tools_test.rs`.
  Equivalent tests landed in `kernel-workspace-local` where the tools now
  live. `main.rs` lost `connect_and_setup`'s tool-schema path, both TUI
  and REPL reader threads' `ExecuteTool` dispatch branches, and
  `DistributionSettings::enabled_tools`. The distro now only knows the
  workspace tool names via `kernel_workspace_local::TOOL_NAMES` for system
  prompt rendering — an intentional coupling that will be replaced by a
  proper query protocol in a later spec.

- **Verify loop (final run)**:
  - `cargo fmt -- --check` — clean
  - `cargo clippy --workspace --all-targets` — exit 0 (one pre-existing
    `while_let_loop` warning in `kernel-core/src/event_loop.rs`, not
    touched by this spec)
  - `cargo test --workspace` — all green
- **Test counts**:
  - `kernel-interfaces`: 33 unit (+4: `null_ctx_drops_chunks_silently`,
    `with_sink_ctx_forwards_chunks`, `parses_manifest_with_toolset_section`,
    `parses_manifest_with_multiple_toolsets`, `legacy_tools_section_fails_to_parse`,
    `missing_toolset_section_is_empty_vec`, minus the two old `tools` tests)
  - `kernel-core`: 72 unit (+1: `streaming_tool_chunks_reach_frontend_in_order`)
    and 15 e2e (unchanged)
  - `kernel-daemon`: 9 unit (+4: `unknown_kind_is_hard_error`,
    `collision_across_toolsets_fails_build`,
    `default_registry_has_workspace_local`,
    `pool_with_workspace_local_produces_tools`; renamed
    `router_register_tools_and_create_session` →
    `router_create_session_without_register_tools`)
  - `kernel-workspace-local`: 7 unit (brand new crate)
  - `kernel-providers`: 0 (unchanged)
  - `dist-code-agent`: 0 (tests/tools_test.rs deleted, no new tests added
    — this distro is now a pass-through shell)

- **Doc-sync** was run via a subagent; findings folded into this commit.
  See commit message for the architecture.md / roadmap.md deltas.
