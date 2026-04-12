# Design Evolution Notes

**Date**: 2026-04-07 (updated 2026-04-08)
**Status**: Proposed changes from architecture stress-testing session

These are proposed modifications to the v0.1 spec based on a critical review of the kernel concept, market research, and architectural exploration. Nothing here is final — these are design directions to evaluate.

---

## 1. Tool Ownership and Enforcement

### Problem
The current architecture has the distro owning tool implementations. The distro registers tool schemas with the kernel, the kernel sends `ExecuteTool` events back to the distro, and the distro executes locally. The kernel never has tool implementations — only schemas and proxy wrappers.

This means:
- Permission enforcement is cooperative, not architectural. The distro can execute tools without asking the kernel.
- Session checkpointing can't serialize tools — they're trait objects in the distro's process.
- If the kernel spawns worker sessions (§5), every worker's tool call must round-trip back to the distro.

### Target Architecture
The kernel is the mandatory proxy for all tool execution. Tools are external services, not distro code. The kernel connects to tool sources and mediates every call through its permission evaluator.

Tools enter the kernel two ways:

- **MCP servers** (target): The distro sends `ConnectToolServer` with a URI. The kernel spawns/connects to the MCP server, discovers tools via `tools/list`, and dispatches `tools/call` directly. The distro is never involved in tool execution.
- **Inline tools** (compatibility shim): The distro sends `RegisterTools` with schemas and handles `ExecuteTool` events, exactly as today. Kept for tools tightly coupled to the distro (e.g., UI-specific tools) and as a migration path.

Both are wrapped as `ProxyTool` with a `ToolSource` enum. The turn loop calls `tool.execute()` on `Box<dyn ToolRegistration>` and doesn't know the source.

```rust
enum ToolSource {
    Inline {
        event_tx: Sender<KernelEvent>,
        response_rx: Receiver<ToolResponse>,
    },
    Mcp {
        client: McpClient,
    },
}
```

### MCP tool annotations
MCP tools don't carry `CapabilitySet`, `TokenEstimate`, or `RelevanceSignal` — these are kernel concepts. The distro provides annotations at connection time:

```rust
KernelRequest::ConnectToolServer {
    session_id: SessionId,
    uri: String,
    transport: Transport,
    annotations: HashMap<String, ToolAnnotation>,
}
```

Policy files can override capability annotations, since the policy is the trust boundary.

### Built-in tool packs
To avoid requiring every distro to manage MCP server processes, the kernel ships built-in tool packs. A distro opts in with one line:

```rust
KernelRequest::CreateSession {
    config: SessionCreateConfig {
        tool_packs: vec!["code-tools"],
        // ...
    },
}
```

The kernel daemon owns the MCP server binary, spawns it, connects, discovers tools, applies default annotations. The distro never thinks about URIs or processes. `ConnectToolServer` remains available as an escape hatch for custom tools.

### What changes in the codebase
- `proxy_tool.rs`: Add `ToolSource` enum with `Inline` and `Mcp` variants.
- `protocol.rs`: Add `ConnectToolServer` request. Make `RegisterTools` session-scoped.
- `router.rs`: Handle `ConnectToolServer`, create MCP-backed ProxyTools. Track tool sources per-session.
- New `mcp_client.rs`: MCP JSON-RPC client (`tools/list`, `tools/call`).
- `dist-code-agent/tools.rs`: Moves to a standalone `mcp-code-tools` crate (separate binary).
- `dist-code-agent/main.rs`: Becomes pure UI — send input, render output, handle permission prompts.

### What does NOT change
The core kernel is untouched: `turn_loop.rs`, `session.rs`, `context.rs`, `permission.rs`. The work is entirely in the tool source layer and daemon router.

### When to build
Don't build this now. The current inline model works for one distro with 5 tools. Build the MCP path when either:
- Sub-agent spawning (§5) needs workers to access tools without round-tripping to the distro
- A second distro needs to reuse the same tools

### Status
Resolved. Spec 0015 moved tools out of the distro entirely: the six filesystem tools now live in a `kernel-workspace-local` library crate, the binary builds a `ToolsetPool` at startup from `[[toolset]]` manifest entries, and snapshots `pool.tools_for_session()` at session create. The `RegisterTools`, `ExecuteTool`, `ToolResult`, and `ToolSchema` protocol variants are deleted; `proxy_tool.rs` and `in_process.rs` are gone; dispatch is kernel-owned, and a new `KernelEvent::ToolCompleted` notifies frontends when a tool finishes. The `ToolSource` enum sketched above never materialized — the seam is instead the `ToolSet` trait plus a `FactoryRegistry` keyed on `kind`. Spec 0016 completed the MCP transport half: the sole factory registered in `default_registry()` is now `"mcp.stdio" → kernel_core::mcp_stdio::from_entry`, which spawns a subprocess (typically `kernel-workspace-local`) and proxies `tools/call` over newline-delimited JSON-RPC 2.0 on the child's stdio. Streaming tool output crosses the channel via `KernelEvent::ToolOutputChunk`. Spec 0017 collapsed the daemon into a single `agent-kernel` binary — `kernel-daemon` is deleted, `ToolsetPool` moved to `kernel-core`, and all IPC plumbing (Unix socket, framing, connection router) is removed. Adding a third-party MCP server is now "ship the binary, add a `[[toolset]]` entry" — zero kernel changes.

---

## 2. Long-Running Sessions

### Problem
Most agent frameworks assume short-lived interactions — user asks, agent responds, session ends. The current v0.1 spec doesn't explicitly commit to either model.

### Design Commitment
The kernel is explicitly designed around the assumption that **agent sessions run for hours, days, or indefinitely**. This is the foundational design commitment, not an afterthought.

### Implications

- **State is the product, not a side effect.** Losing a long-running session's accumulated context means losing hours of compute and reasoning. Checkpointing becomes a correctness requirement, not a scaling feature.
- **The environment changes under you.** Files change, CI results arrive, dependencies update. The invalidation system (`Invalidation::Files`, `Invalidation::Environment`) is the mechanism that keeps a long-running process coherent with reality.
- **Compaction is continuous, not occasional.** A long-running session will hit the context window repeatedly. Compaction is the garbage collector, running throughout the process's life.
- **Failure recovery is non-negotiable.** Sessions must survive process restarts, network partitions, policy changes, and provider migrations.
- **Resource management is about sustainability, not throughput.** Token budgets become rates, not caps.

### Design consequences

| Component | Short-lived assumption | Long-running assumption |
|---|---|---|
| Context compaction | Nice to have | Core infrastructure |
| Session serialization | Scaling feature | Survival mechanism |
| Invalidation system | Cleanup | Correctness |
| Channels | Future work | Essential input |
| Token budgets | Hard cap | Rate limiter |
| Scratchpad (Tier 1) | Convenience | Institutional memory |
| Pending results | Queue | Event loop |

### Status
Resolved by spec 0004. `ContextManager::compact()` now takes a `&dyn ProviderInterface` and calls `provider.complete(...)` with a hard-coded compaction prompt to produce a real 2-3 sentence summary per turn, preserving concrete facts and dropping incidental detail. The old 100-char `summarize_turn()` truncation stub is gone. Compaction is a projection over the in-memory view — the Tier-3 event stream (spec 0003) is untouched, so the authoritative history remains byte-for-byte intact and can be re-derived later (replay/hydration is spec 0005's concern).

One level of model-based summarization on focused context is acceptable — the pathology is compounding chains of re-summarization, which the session tree structure (§5) eliminates.

---

## 3. Session Checkpointing

### Problem
If a session lives for hours/days, it must survive restarts, machine failures, and migration between environments. The original implementation held all state in memory with no persistence.

### Status
Partially resolved. Spec 0003 added the append-only event stream (`SessionEventSink`, `FileSink`, `events.jsonl`) so every `append_*` on the context manager is durably recorded. Spec 0005 added the read path: `session_events::read_events_from_file`, `ContextManager::replay_events` / `hydrated_from_events`, and `SessionManager::hydrate_from_events`. A session can now be rebuilt in-memory from a local `events.jsonl` by replaying events through the same `append_*` methods that wrote them (a `NullSink` prevents re-emission).

Spec 0006 moved the default on-disk location from `<workspace>/.agent-kernel/session-{id}/events.jsonl` to `<base>/sessions/{id}/events.jsonl`, where `<base>` is `$AGENT_KERNEL_HOME`, else `$HOME/.agent-kernel`, else `./.agent-kernel`. Sessions are now globally addressable — the workspace is preserved as metadata inside the `SessionStarted` event, not as filesystem structure — which closes out the "find any past session without knowing its workspace" prerequisite for the future session store.

Spec 0007 added a one-way remote audit path: `HttpSink` POSTs each event as JSON to `<AGENT_KERNEL_REMOTE_SINK_URL>/events` (with optional `AGENT_KERNEL_REMOTE_SINK_TOKEN` bearer auth), and `TeeSink<A, B>` fans `record()` out to both the local `FileSink` and the remote `HttpSink` when the env var is set. No new dependencies — the client is a ~40-line `std::net::TcpStream` HTTP/1.1 implementation, `http://` only. Delivery is best-effort: failed POSTs bump a counter and log to stderr but never block the turn loop. The kernel still runs locally; only the audit stream goes remote.

What's still deferred: the full `SessionSnapshot` shape below (scratchpad, tokens_used, compaction state, pending_results, parent/children, tool sources, metadata) is not yet in the event schema — hydration today reconstructs the turn view only, and non-historical config (policy, tools, completion config, resource budget) is caller-provided at hydrate time. Remote hydration (reading the event stream back from a remote store), true remote session execution, HTTPS, retry/backoff, and batching are all out of scope for spec 0007 — see spec 0008 for remote execution.

### Proposed Change
Sessions are serializable to a `SessionSnapshot` and can be checkpointed, stored, and hydrated on demand.

```rust
struct SessionSnapshot {
    // Identity
    id: SessionId,
    mode: SessionMode,
    workspace: PathBuf,

    // Context state (bulk of the snapshot)
    system_prompt: String,
    scratchpad: Scratchpad,
    turns: Vec<Turn>,
    tokens_used: usize,
    compaction_failures: u32,

    // Execution state
    next_turn_id: u64,

    // Policy
    policy: Policy,

    // Config
    context_config: ContextConfig,
    completion_config: CompletionConfig,
    max_tool_invocations_per_turn: usize,
    max_tokens: usize,

    // Pending work
    pending_results: Vec<PendingResult>,

    // Parentage
    parent: Option<SessionId>,
    children: Vec<SessionId>,

    // Tool sources (what to reconnect on hydrate)
    tool_sources: Vec<ToolSourceConfig>,

    // Metadata
    created_at: DateTime<Utc>,
    last_active: DateTime<Utc>,
    total_tokens_consumed: usize,
    total_turns_completed: u64,
}

enum ToolSourceConfig {
    Inline { schemas: Vec<ToolSchema> },
    Mcp { uri: String, transport: Transport },
}
```

### What is NOT serialized (reconstructed on hydrate)
- `file_cache` — re-readable from disk
- `tool_definitions_in_context` — rebuilt from tool registry
- `tool_names_in_context` — rebuilt from tool registry
- `last_compaction_time` — reset to None
- `tools` — reconstructed from `tool_sources`: kernel reconnects to MCP servers, waits for distro to re-register inline tools

### Serialization challenge
The current `Session` holds `tools: Vec<Box<dyn ToolRegistration>>` and `context: ContextManager` with `Box<dyn ContextStore>`. Trait objects aren't `Serialize`. The snapshot approach sidesteps this by extracting serializable data from the session rather than serializing the session directly. On hydrate, the kernel reconstructs the non-serializable components from the serialized config.

### Snapshot vs full history
Ship the full snapshot as a single JSON blob first. A git-like model (immutable turns, content-addressable, delta-based sync) is architecturally cleaner — turns are append-only, so syncing means transferring only the turns the other side doesn't have. But it's not worth building until snapshots are large enough or transfers happen often enough to justify the complexity.

---

## 4. Session Store

### Problem
Session persistence needs to work for local development and single-instance deployments. Multi-instance coordination (ownership, distributed locking) is a future concern — don't design for it now.

### Proposed Change
Session storage is abstracted behind a minimal `SessionStore` trait:

```rust
trait SessionStore {
    fn save(&self, id: SessionId, snapshot: SessionSnapshot) -> Result<()>;
    fn load(&self, id: SessionId) -> Result<SessionSnapshot>;
    fn exists(&self, id: SessionId) -> bool;
    fn list(&self, filter: SessionFilter) -> Result<Vec<SessionMeta>>;
    fn delete(&self, id: SessionId) -> Result<()>;
}
```

Ownership semantics (`claim`/`release`) are intentionally omitted — they only matter for multi-instance coordination, which has no demonstrated demand yet. The trait can be extended when that need materializes.

### Implementation
**FileStore** — `~/.agent-kernel/sessions/{id}.json`. Zero infrastructure. Ship this first and only.

Future backends (Postgres, remote API) can be added when the deployment model demands them. The trait is stable enough to extend without breaking existing implementations.

When session count grows, the `SessionManager` can use the store for LRU eviction — active sessions in memory, idle sessions serialized to the store with only metadata in the index. This falls out naturally from having the store; no separate scaling design needed.

### Configuration
```yaml
session_store:
  backend: file
  path: ~/.agent-kernel/sessions
```

---

## 5. Session Trees and Structural Compaction

### Problem
Traditional compaction (text summarization) is lossy, expensive, compounds errors across multiple summarization passes, and can't predict what information will matter later.

### Core Idea
The primary compaction mechanism is **structural**: work is decomposed into a tree of coordinator and worker sessions. Each worker produces a bounded summary as a natural byproduct of completing. The coordinator holds only summaries, never the workers' internal reasoning.

```
Coordinator: "Refactor the auth module"
  ├── Worker A: "Analyze current auth code"
  │   Runs 15 turns, reads 8 files.
  │   Completes → returns 200-token summary.
  │   Worker A's 15 turns of context: DISCARDED.
  │
  ├── Worker B: "Design new auth interface"
  │   Gets Worker A's summary.
  │   Runs 10 turns.
  │   Completes → returns 250-token summary.
  │
  └── Worker C: "Implement the migration"
      Gets Worker A + B summaries.
      Runs 30 turns, writes code, tests.
      Completes → returns 300-token summary.

Coordinator's total context: ~1,500 tokens
Total work performed: 55 turns, 150K+ tokens
Compression ratio: ~99%, zero summarization calls
```

Note: the 99% compression applies to the coordinator's context window, not total token cost. All worker tokens are still consumed at generation time. The win is that the coordinator stays coherent across large tasks instead of drowning in accumulated context.

### Why this is better than text summarization
1. **The worker writes its own summary.** Its final response IS the summary, produced with full context of what it found. No separate summarization call.
2. **No compounding errors.** Each summary is first-generation — derived from full context, not from a previous summary.
3. **Zero summarization cost.** No extra model calls. The worker's completion response is the compaction.
4. **Natural boundaries.** Cuts at task completion, not arbitrary turn counts.
5. **Parallelism.** Independent workers can run concurrently.

### Session tree lifecycle

**Spawn.** The model calls a kernel-provided `spawn_worker` tool:

```json
{ "task": "Analyze the auth module", "context": "We're refactoring..." }
```

The kernel handles this tool directly (not dispatched to the distro). It:
- Creates a child session via `SessionManager`
- Sets the child's system prompt from the task description
- Seeds the child's context with the provided context string
- Inherits tools from the coordinator (or a policy-defined subset)
- Inherits policy from the coordinator (or a restricted version)
- Sets a context budget (fraction of coordinator's remaining budget, or explicit in spawn call)
- Records the parent → child relationship on both sessions

**Run.** The worker runs autonomously with its own turn loop, context, and token budget. Two modes:

- **Blocking**: `spawn_worker` doesn't return until the worker completes. The coordinator's turn loop is suspended at the tool dispatch step. Simple, good for sequential decomposition.
- **Non-blocking**: `spawn_worker` returns immediately with a worker ID. The worker runs concurrently. When it completes, the result arrives as `PendingResult::ChildCompleted` and gets injected before the coordinator's next turn. Good for parallel work.

**Complete.** The worker's turn loop runs until `continues == false`. The kernel:
- Captures the final text response as the worker's summary
- Delivers it to the coordinator (as tool result if blocking, as `PendingResult::ChildCompleted` if non-blocking)
- Propagates invalidations (files written, etc.) to the coordinator's context
- Optionally checkpoints the worker session for audit/replay

**Cleanup.** The worker's context (all intermediate turns) is discarded. The kernel either drops the session from memory, moves it to idle in the session store, or keeps it for inspection. The coordinator retains only the summary.

**Failure.** Workers can fail:

| Failure | Kernel response |
|---|---|
| Exceeds token budget | Kill, return error summary to coordinator |
| Exceeds turn limit | Kill, return partial results to coordinator |
| Provider error | Retry or propagate to coordinator |
| Tool error | Worker sees it, can self-recover |
| Coordinator cancelled | Cascade cancel to all children via `cancelled` flag |

The coordinator model sees failures as tool results with error messages. It can retry with a different decomposition, handle the subtask itself, or give up.

### Kernel controls

The model decides WHEN to spawn workers. The kernel enforces:
- **Max children** per coordinator (budget)
- **Max depth** of the session tree (prevents unbounded recursion)
- **Budget per child** (token budget, turn limit)
- **Policy inheritance** (children can't escalate permissions)
- **Minimum utilization** for spawning (reject trivial spawns when context utilization is low)

### Text summarization as fallback
Model-based compaction remains available for leaf-node workers that can't decompose further. One level of summarization on a focused subtask is acceptable — the pathology is compounding chains, which the tree structure eliminates.

### Bootstrap problem
Structural compaction requires multi-session support, which is the largest piece of v0.2 work. The existing `PendingResult::ChildCompleted` delivery mechanism exists, but nothing creates children, manages their lifecycle, or connects results back. Model-based leaf-node compaction (§2) should be built first — it's needed regardless, and it's useful without the tree.

---

## 6. Remote Execution and Migration

### Problem
Long-running agent sessions shouldn't require a laptop to stay open. Developers want to start work locally, hand it off to a remote environment, and check back later.

### Remote execution
The kernel daemon runs on any host that can reach the model API — a cloud VM, Render, Fly, a k8s pod. The requirements:

- **TCP socket** instead of Unix socket (or both, selectable)
- **Authentication** on the socket (the current daemon has none)
- **Persistent volume** for the workspace (the agent needs files to work with)
- **Session checkpointing** (§3) so sessions survive container restarts

The distro becomes a thin remote client. The TUI connects to `render-host:port` instead of `/tmp/agent-kernel.sock`, but speaks the same protocol. The user checks in when they want, sees progress, steers the session, disconnects. The session keeps running.

A `--remote` flag on the distro is the entry point:

```bash
agent-kernel --remote render-host:9000
```

### Session migration
Moving a session between local and remote:

1. Source kernel checkpoints the session (§3)
2. Snapshot transfers to the target (direct transfer or shared store)
3. Workspace syncs via git (push from source, pull on target)
4. Target kernel hydrates the session
5. Distro reconnects to the target daemon

The session snapshot carries full state: scratchpad, token accounting, compaction state, pending results, parent/child relationships. This is more complete than conversation-history-only approaches (like teleport), which lose the agent's accumulated working state.

### Workspace sync
The hard part of migration isn't the session — it's the workspace. The session references files that must exist on the target. Git is the natural transport: push before migrate, pull on the other side. The kernel could automate this as part of the migration flow (push, snapshot, transfer, pull, hydrate), but the first implementation should be manual (`git push` yourself, then migrate).

**Status:** The minimum-viable primitive shipped in spec 0008. `SessionEvent::SessionStarted` now carries an optional `WorkspaceFingerprint` (commit, branch, dirty, absolute path) captured via `fingerprint_workspace` at session-create time, and `SessionManager::hydrate_from_events` accepts a `verify_workspace` flag that rejects hydration on commit mismatch. This doesn't move any files — it's purely a safety rail so the manual "git push / git pull / hydrate" workflow notices when the target workspace is on the wrong commit. Full automated push/pull/verify is still future work.

### When to build
Remote execution (TCP socket + auth) is useful on its own without migration — it lets you run headless agents on a server. Build that first. Migration is a natural follow-on once checkpointing works.

---

## 7. Fields Missing from Current Implementation

The following fields should be added to support the long-running process model:

### Session
- `parent: Option<SessionId>` — who spawned this session
- `children: Vec<SessionId>` — sessions this one has spawned
- `created_at: DateTime<Utc>` — for idle detection and session expiry
- `last_active: DateTime<Utc>` — for LRU eviction
- `total_tokens_consumed: usize` — lifetime accounting (survives compaction)
- `total_turns_completed: u64` — lifetime counter

### ContextConfig (existing but unenforced)
- `max_tokens` — currently `#[allow(dead_code)]`, should be enforced in the turn loop

### PolicyRule (existing but unevaluated)
- `scope_paths` and `scope_commands` — defined in the policy schema but never checked in `Policy::evaluate()`. Implementing these requires a signature change: `evaluate()` currently takes `&Capability`, but path/command scoping requires access to the tool's input at evaluation time. Either change the signature to accept tool input, or remove the fields.

---

## 8. What to De-emphasize

Based on market research and architecture review:

- **Federation** (multi-kernel coordination, shared rate limiting, distributed invalidation) is architecturally clean but has zero demonstrated demand. It was removed from this document. The session store trait can be extended for it later.
- **The "distribution" concept** needs a concrete proof point. Two different distributions sharing meaningful kernel infrastructure, not just "two apps using the same library."
- **Tool dispatch abstraction** is being commoditized by MCP. The kernel should host MCP servers, not compete with MCP.
- **Provider abstraction** is a thin wrapper problem, not a kernel problem. Keep it minimal.
- **Scaling session count** beyond LRU eviction (turn schedulers, global token budgets, priority queues) is speculative. Build it when session count actually grows.
- **Focus the narrative** on structural compaction, session lifecycle (checkpoint/migrate/resume), and the coordinator/worker pattern. These are genuinely differentiated.
