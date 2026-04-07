# Design Evolution Notes

**Date**: 2026-04-07
**Status**: Proposed changes from architecture stress-testing session

These are proposed modifications to the v0.1 spec based on a critical review of the kernel concept, market research, and architectural exploration. Nothing here is final — these are design directions to evaluate.

---

## 1. Kernel as Binary, Not Just Library

### Problem
The current implementation is a Rust library (`cargo add kernel-core`). Distributions link directly to kernel code. This means there's no real protection boundary — a distribution can bypass the permission evaluator by calling `tool.execute()` directly. The "kernel" is advisory, not enforced.

### Proposed Change
The kernel should support two deployment modes transparently:

- **Library mode** (`KernelHandle::Local`): Direct function calls, same process. Zero overhead. Good for development, testing, and single-binary deployments.
- **Binary mode** (`KernelHandle::Remote`): Kernel runs as a daemon. Distributions communicate via IPC (JSON-RPC over Unix socket). The distribution never has a handle to tool implementations — it sends requests, the kernel dispatches. Permission enforcement is architectural, not conventional.

Both modes implement the same `KernelInterface` trait:

```rust
enum KernelHandle {
    Local(Session),
    Remote(KernelClient),
}

trait KernelInterface {
    fn run_turn(&mut self, input: &str) -> Result<TurnResult, TurnError>;
    fn deliver(&mut self, event: PendingResult);
    fn set_policy(&mut self, policy: Policy);
    fn context_status(&self) -> ContextStatus;
}
```

### Rationale
- Library mode lets users adopt incrementally with zero operational overhead.
- Binary mode enables real process isolation, multi-tenant resource sharing, and session-outlives-client semantics.
- The traits in `kernel-interfaces` are already request/response shaped — they map naturally to a wire protocol.
- Strategy: ship as library first, evolve to binary when users pull for it.

### Impact
- `kernel-interfaces` traits become the wire protocol spec.
- New crate: `kernel-client` (thin IPC client implementing `KernelInterface`).
- `kernel-core` gains a server mode (listen on socket, host sessions).
- Existing library API remains unchanged.

---

## 2. Long-Running Processes as Core Design Assumption

### Problem
Most agent frameworks assume short-lived interactions — user asks, agent responds, session ends. The current v0.1 spec doesn't explicitly commit to either model.

### Proposed Change
The kernel is explicitly designed around the assumption that **agent sessions run for hours, days, or indefinitely**. This is the foundational design commitment, not an afterthought.

### Implications

- **State is the product, not a side effect.** Losing a long-running session's accumulated context means losing hours of compute and reasoning. Checkpointing becomes a correctness requirement, not a scaling feature.
- **The environment changes under you.** Files change, CI results arrive, dependencies update. The invalidation system (`Invalidation::Files`, `Invalidation::Environment`, channels) is the mechanism that keeps a long-running process coherent with reality.
- **Compaction is continuous, not occasional.** A long-running session will hit the context window repeatedly. Compaction is the garbage collector, running throughout the process's life.
- **Failure recovery is non-negotiable.** Sessions must survive process restarts, network partitions, policy changes, and provider migrations.
- **Resource management is about sustainability, not throughput.** Token budgets become rates, not caps. The turn scheduler paces sessions across their full lifetime.

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

---

## 3. Session Checkpointing and Migration

### Problem
If a session lives for hours/days, it must survive restarts, machine failures, and deployment. The current implementation holds all state in memory with no persistence.

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

    // Metadata
    created_at: DateTime<Utc>,
    last_active: DateTime<Utc>,
    total_tokens_consumed: usize,
    total_turns_completed: u64,
}
```

### What is NOT serialized (reconstructed on hydrate)
- `file_cache` — re-readable from disk
- `tool_definitions_in_context` — rebuilt from tool registry
- `tool_names_in_context` — rebuilt from tool registry
- `last_compaction_time` — reset to None
- `tools` — injected by the kernel from its registry

### Migration support
Sessions can migrate between kernel instances (local → remote, pod → pod) via:
1. **Snapshot and transfer** — serialize, send to target, hydrate
2. **Replay** — transfer the input log, replay into fresh session
3. **Lazy migration** — compact current context into summary, seed new session

---

## 4. Session Store Behind a Trait (and Optionally an API)

### Problem
Session persistence needs to work across deployment modes — local dev, self-hosted, cloud, federated kernels.

### Proposed Change
Session storage is abstracted behind a `SessionStore` trait:

```rust
trait SessionStore {
    fn save(&self, id: SessionId, snapshot: SessionSnapshot) -> Result<()>;
    fn load(&self, id: SessionId) -> Result<SessionSnapshot>;
    fn exists(&self, id: SessionId) -> bool;
    fn list(&self, filter: SessionFilter) -> Result<Vec<SessionMeta>>;
    fn delete(&self, id: SessionId) -> Result<()>;
    fn claim(&self, id: SessionId, owner: KernelId) -> Result<bool>;
    fn release(&self, id: SessionId) -> Result<()>;
}
```

### Implementations
1. **FileStore** — `~/.agent-kernel/sessions/{id}.json`. Zero infrastructure. Default for dev.
2. **PostgresStore** — direct database connection. Self-hosted production.
3. **ApiStore** — HTTP client to a remote session store service.

### API surface (for remote store)
```
POST   /v1/sessions                            # create
GET    /v1/sessions/{id}                       # load snapshot
PUT    /v1/sessions/{id}                       # save snapshot
DELETE /v1/sessions/{id}                       # delete
GET    /v1/sessions?owner={kernel}&state=idle  # list/filter
POST   /v1/sessions/{id}/claim                 # take ownership (atomic)
POST   /v1/sessions/{id}/release               # release ownership
GET    /v1/sessions/{id}/meta                  # metadata only
WebSocket /v1/events?kernel={id}               # invalidation + event stream
```

### Configuration
```yaml
session_store:
  backend: file | postgres | api
  path: ~/.agent-kernel/sessions        # file backend
  url: postgres://... or https://...    # postgres/api backend
  auth:
    token_env: AGENT_KERNEL_STORE_TOKEN  # api backend
```

---

## 5. Structural Compaction via Session Trees

### Problem
Traditional compaction (text summarization) is lossy, expensive, compounds errors across multiple summarization passes, and can't predict what information will matter later.

### Proposed Change
The primary compaction mechanism is **structural**: work is decomposed into a tree of coordinator and worker sessions. Each worker produces a bounded summary as a natural byproduct of completing. The coordinator holds only summaries, never the workers' internal reasoning.

### How it works
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

### Why this is better than text summarization
1. **The worker writes its own summary.** Its final response IS the summary, produced with full context of what it found. No separate summarization call.
2. **No compounding errors.** Each summary is first-generation — derived from full context, not from a previous summary.
3. **Zero summarization cost.** No extra model calls. The worker's completion response is the compaction.
4. **Natural boundaries.** Cuts at task completion, not arbitrary turn counts.
5. **Parallelism.** Independent workers can run concurrently.

### The kernel's role in decomposition
The model decides WHEN to spawn workers. The kernel decides:
- How many children a coordinator can have (budget)
- What policy children inherit
- What provider/model children use
- When to kill a child that exceeds its budget
- How to deliver results back to the parent
- How to cascade invalidations through the tree

### Encouraging good decomposition
The kernel creates pressure toward decomposition without making the decision itself:
- **Context pressure**: Tight per-session context budgets make decomposition necessary.
- **System prompt**: Decomposition guidelines injected by the kernel.
- **Rejection of trivial spawns**: Kernel can refuse spawns when context utilization is low.
- **Budget enforcement**: Workers that exceed budgets fail fast — bad decompositions are self-correcting.

### Text summarization as fallback
Traditional compaction remains available for leaf-node workers that can't decompose further. One level of summarization on a focused subtask is acceptable — the pathology is compounding chains, which the tree structure eliminates.

---

## 6. Scaling Session Count

### Problem
The current `SessionManager` is `Vec<Session>` — all sessions in memory, no scheduling, no shared resource management.

### Proposed Change (staged)

**Stage 1: Session swap.** Sessions have three states: Active (in memory), Idle (serialized to store, metadata only in memory), Suspended (checkpointed, near-zero footprint). The kernel maintains a working set with LRU eviction.

```rust
struct SessionManager {
    active: HashMap<SessionId, Session>,
    index: HashMap<SessionId, SessionMeta>,
    store: Box<dyn SessionStore>,
    max_active: usize,
}
```

**Stage 2: Turn scheduler.** Provider API calls go through a priority queue. The kernel owns the provider, not individual sessions. Interactive sessions get priority over autonomous/background sessions.

```rust
struct TurnScheduler {
    queue: PriorityQueue<TurnRequest>,
    in_flight: usize,
    max_concurrent: usize,
    policy: SchedulingPolicy,
}
```

**Stage 3: Global token budgets.** Cumulative token usage tracked across all sessions. Per-session and per-hour caps enforced by the kernel. This is cgroups for tokens.

**Stage 4: Multi-instance.** Shared session store, stateless routing, distributed invalidation. The `SessionManager` interface doesn't change — the implementation moves from in-process to distributed.

---

## 7. Local/Remote Kernel Switching

### Problem
Developers want library-mode ergonomics locally and daemon-mode isolation in production.

### Proposed Change
A `KernelHandle` enum that transparently switches between local and remote:

```rust
enum KernelHandle {
    Local(Session),
    Remote(KernelClient),
    Auto,  // try remote, fall back to local
}
```

Both implement `KernelInterface`. The distribution codes against the trait and doesn't know which variant it's using.

### Migration on the fly
Switching from local to remote mid-session:
1. Kernel checkpoints session to the shared store
2. Remote kernel hydrates from the store
3. Handle switches from `Local` to `Remote`
4. Next `run_turn()` goes over IPC — distribution doesn't notice

---

## 8. Kernel Federation

### Problem
Production deployments span multiple environments — developer laptops, CI, staging, production k8s clusters.

### Proposed Change
Kernel instances can federate by sharing a session store and invalidation bus. A local kernel registers as a peer in the same hash ring as k8s kernel pods.

### Capabilities
- Shared session store: any kernel can hydrate any session
- Shared rate limiting: provider budget coordinated via Redis/shared token bucket
- Cross-kernel invalidation: file changes propagate across all kernels
- Session migration: move sessions between any kernels in the federation
- Role-based participation: edge kernels (laptop, limited resources) vs core kernels (k8s, full capacity)

### Conflict resolution
Optimistic locking on session ownership in the store. If two kernels claim the same session, one wins, the other backs off. Conflicts are rare (only during network partitions + simultaneous migration).

---

## 9. Fields Missing from Current Implementation

The following fields should be added to support the long-running process model:

### Session
- `parent: Option<SessionId>` — who spawned this session
- `children: Vec<SessionId>` — sessions this one has spawned
- `created_at: DateTime<Utc>` — for idle detection and session expiry
- `last_active: DateTime<Utc>` — for LRU eviction
- `total_tokens_consumed: usize` — lifetime accounting (survives compaction)
- `total_turns_completed: u64` — lifetime counter

### ContextConfig (existing but unenforced)
- `max_tokens` — currently `#[allow(dead_code)]`, should be enforced

### PolicyRule (existing but unevaluated)
- `scope_paths` and `scope_commands` — defined in the policy schema but never checked in `Policy::evaluate()`. Either implement evaluation or remove the fields.

---

## 10. What to De-emphasize

Based on market research and architecture review:

- **The "distribution" concept** needs a concrete proof point. Two different distributions sharing meaningful kernel infrastructure, not just "two apps using the same library."
- **Federation** is architecturally clean but has zero demonstrated demand. Design for it, don't build it until pulled.
- **Tool dispatch abstraction** is being commoditized by MCP. The kernel should host MCP servers, not compete with MCP.
- **Provider abstraction** is a thin wrapper problem, not a kernel problem. Keep it minimal.
- **Focus the narrative** on structural compaction, session lifecycle (checkpoint/migrate/resume), and the coordinator/worker pattern. These are genuinely differentiated.
