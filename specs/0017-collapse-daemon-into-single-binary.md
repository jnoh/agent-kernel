---
id: 0017-collapse-daemon-into-single-binary
status: done
---

# Collapse daemon into single binary

## Goal

Merge `kernel-daemon` into `dist-code-agent`, producing a single binary
(`agent-kernel`) that loads the manifest, builds the provider and toolset
pool, creates sessions, and runs the TUI — all in one process with no
Unix socket IPC. The `kernel-daemon` crate is deleted. The renamed crate
`agent-kernel` (previously `dist-code-agent`) becomes the only binary
crate in the workspace.

## Motivation

The daemon/distro split added a Unix socket protocol, length-prefixed
framing, a connection router, serialization of every event, and a
two-process startup ceremony — all to support a multi-distro,
multi-session future that doesn't exist and wouldn't use this protocol
if it did (a web frontend would use WebSocket, not Unix sockets).
Collapsing removes ~800 lines of IPC plumbing, halves the startup
steps, and makes every `FrontendEvents` call a direct function call
instead of a serialize → socket → deserialize round-trip. The traits
in `kernel-interfaces` and the turn loop in `kernel-core` are unchanged
— the extension points that would support a future web frontend or
multi-session server survive intact.

## Context

- `crates/kernel-daemon/src/main.rs` — daemon entry point: loads
  manifest, builds provider factory + toolset pool, accepts Unix
  socket connections, spawns reader/writer threads.
- `crates/kernel-daemon/src/router.rs` — `ConnectionRouter`: handles
  `CreateSession` by snapshotting tools, building `ProxyFrontend`,
  constructing `EventLoopConfig`, spawning `EventLoop` thread. Routes
  `AddInput`, `PermissionResponse`, etc. to per-session channels.
- `crates/kernel-daemon/src/manifest.rs` — `ProviderFactory` type and
  `build_provider_factory()`. Re-exports manifest types. Turns
  `ProviderConfig` into Arc'd closure.
- `crates/kernel-daemon/src/toolset_pool.rs` — `ToolsetPool`,
  `FactoryRegistry`, `default_registry()`. Already depends only on
  `kernel-interfaces` + `kernel-core`.
- `crates/dist-code-agent/src/main.rs` — current client: connects to
  socket, sends `CreateSession`, spawns reader thread, runs TUI loop.
  `connect_and_setup()` is the function that goes away.
- `crates/dist-code-agent/Cargo.toml` — current deps. Needs
  `kernel-core`, `kernel-providers`, `crossbeam-channel`, `toml` added.
  `kernel-workspace-local` dep stays (for `TOOL_NAMES`).
- `crates/kernel-core/src/event_loop.rs` — `EventLoop` + `EventLoopConfig`.
  Already communicates via crossbeam channels. Unchanged by this spec.
- `crates/kernel-core/src/proxy_frontend.rs` — `ProxyFrontend`. Sends
  `KernelEvent` over a crossbeam `Sender`. Unchanged — in the collapsed
  world, the TUI thread receives from the same channel directly instead
  of through a socket.
- `crates/kernel-interfaces/src/framing.rs` — length-prefixed framing
  for Unix socket IPC. No longer used after collapse; delete the module.
- `crates/kernel-interfaces/src/protocol.rs` — `KernelEvent`,
  `KernelRequest`, `SessionCreateConfig`. Keep all of these — they're
  still the channel protocol between the session thread and the TUI
  thread. Only the transport changes (socket → in-process channel).
- `Cargo.toml` (workspace root) — remove `kernel-daemon` from members.
- `distros/code-agent.toml` — unchanged. The single binary reads it via
  `--manifest`.

## Design decisions (locked)

**Rename.** The crate directory moves from `crates/dist-code-agent` to
`crates/agent-kernel`. The `[[bin]]` name stays `agent-kernel` (already
the case). The crate's `[package] name` changes from `dist-code-agent`
to `agent-kernel`.

**ProxyFrontend stays.** The TUI runs on the main thread; the turn loop
runs on a session thread. They still need a channel between them.
`ProxyFrontend` already sends `KernelEvent` over a crossbeam `Sender`,
which is exactly what the TUI reads. No new frontend impl is needed.
The only difference is that the channel connects directly instead of
going through a socket.

**ToolsetPool moves to kernel-core.** `toolset_pool.rs` currently lives
in `kernel-daemon` but only depends on `kernel-interfaces` +
`kernel-core`. Move it to `kernel-core` so the single binary can use
it without depending on a deleted crate. `default_registry()` stays
with it.

**ProviderFactory + build_provider_factory move to agent-kernel.** This
function depends on `kernel-providers` (concrete Anthropic/Echo types),
which is a binary-level concern, not a core concern. It moves into the
new `agent-kernel` crate as a private module.

**Session event sink setup moves to agent-kernel.** The `FileSink` /
`HttpSink` / `TeeSink` wiring from `router.rs` lines 96-131 moves into
the binary's session setup.

**Framing module deleted.** `kernel-interfaces/src/framing.rs` and its
`pub mod framing` in `lib.rs` are removed. No crate uses socket
framing after the collapse.

**CLI changes.** `--socket` flag removed. `--distro` renamed to
`--manifest` (clearer now that there's one binary). `--repl` stays.
The daemon auto-discovery logic in `main.rs` (scanning `/tmp` for
`.sock` files) is deleted.

**KernelRequest/KernelEvent kept.** These enums remain the protocol
between the session thread and the TUI thread. They flow over crossbeam
channels instead of a socket, but the types are unchanged. Renaming
them would be churn.

**No multi-session in this spec.** The binary creates exactly one
session, same as today. Multi-session (web frontend, multiple chats)
is a future spec that will add a session-per-thread spawner — the
collapsed architecture makes that easier, not harder.

## Acceptance criteria

- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes.

### Workspace structure

- [ ] `crates/kernel-daemon/` directory deleted entirely.
- [ ] `crates/dist-code-agent/` renamed to `crates/agent-kernel/`.
- [ ] Workspace root `Cargo.toml` members list updated: no
      `kernel-daemon`, `dist-code-agent` → `agent-kernel`.
- [ ] `crates/agent-kernel/Cargo.toml` package name is `agent-kernel`.
      `[[bin]]` name stays `agent-kernel`. New deps: `kernel-core`,
      `kernel-providers`, `crossbeam-channel`, `toml`.

### ToolsetPool in kernel-core

- [ ] `crates/kernel-core/src/toolset_pool.rs` exists, moved from
      `kernel-daemon`. `pub mod toolset_pool` added to
      `kernel-core/src/lib.rs`.
- [ ] `default_registry()` returns `"mcp.stdio"` factory (same as
      before the move).
- [ ] All existing toolset_pool tests pass in their new home.

### Single-binary session setup

- [ ] `agent-kernel` binary (in `crates/agent-kernel/src/main.rs`)
      loads the manifest, builds the provider factory and toolset pool,
      creates `EventLoopConfig` with `ProxyFrontend`, spawns
      `EventLoop` on a thread, and connects the TUI to the crossbeam
      channels — no socket, no daemon process.
- [ ] `--manifest <path>` replaces `--distro`. The binary still works
      without it (deprecated defaults with warning).
- [ ] `--socket` flag removed. Daemon socket auto-discovery removed.
- [ ] Permission responses route directly to `ProxyFrontend`'s
      `permission_tx` channel (no socket round-trip).
- [ ] REPL mode still works (`--repl` or manifest `[frontend] type =
      "repl"`).
- [ ] All existing `dist-code-agent` tests pass (adjusted for the
      new module paths).

### Cleanup

- [ ] `kernel-interfaces/src/framing.rs` deleted. `pub mod framing`
      removed from `kernel-interfaces/src/lib.rs`. No compilation
      errors from removed module.
- [ ] No remaining references to `kernel-daemon`, `agent-kernel-daemon`,
      `dist-code-agent`, or `connect_and_setup` in any Rust source file.
- [ ] `distros/code-agent.toml` unchanged (still valid, still
      parseable by the binary).

### docs

- [ ] doc-sync subagent covers architecture.md and design-proposals.md.

## Out of scope

- **Multi-session / web frontend.** This spec collapses the process
  boundary; adding new session-spawning modes is a separate spec.
- **Removing KernelEvent/KernelRequest.** These are still the channel
  protocol; renaming or restructuring them is unnecessary churn.
- **Removing ProxyFrontend.** It's still the bridge between the session
  thread and the TUI thread via channels. A direct `TuiFrontend` impl
  would be an alternative design but adds no value — the channel is
  needed regardless.
- **Async/await conversion.** The turn loop is blocking and
  single-threaded per session. This is fine and correct.
- **Changes to kernel-core's turn loop, context manager, or permission
  evaluator.** These are untouched.
- **Changes to kernel-workspace-local.** The MCP subprocess model is
  unchanged.

## Checkpoints

Standing directive: skip checkpoints, execute to completion. Doc-sync
+ verify loop before the final commit.

## Notes

- **Dead code in dist-code-agent.** The old crate had `provider.rs` and
  `frontend.rs` files that were never referenced by `mod` in main.rs —
  leftover from a pre-spec era when providers lived in the distro crate.
  Deleted as part of the rewrite.

- **Test count change.** kernel-interfaces dropped from 33 to 28 tests
  (5 framing tests removed with the module). kernel-core gained 4
  toolset_pool tests (moved from kernel-daemon). kernel-daemon's 9 tests
  were removed with the crate. Net: 159 → 149.

- **crossbeam → mpsc bridge in TUI mode.** The TUI main loop uses
  `std::sync::mpsc::Receiver::try_recv()` for non-blocking event drain.
  ProxyFrontend sends on a `crossbeam_channel::Sender`. A bridging thread
  forwards between them. This could be simplified by switching the TUI to
  crossbeam directly, but it's not worth the churn in this spec.

- **`--distro` kept as deprecated alias.** The arg parser accepts both
  `--manifest` and `--distro` so existing scripts don't break. The
  deprecation warning guides users to the new flag.
