# agent-kernel v0.1 Specification

**A Linux-informed runtime for building AI agent harnesses.**

> The kernel doesn't compete with Claude Code. It competes with building from scratch — which is what every production agent builder actually does today.

---

## 1. What This Is

agent-kernel is a runtime layer for building AI agents. It sits below application-level agent frameworks (LangGraph, CrewAI) and above model APIs and protocols (MCP, A2A). It provides three core primitives that no existing framework unifies: tiered context management, capability-based tool dispatch, and defense-in-depth security.

The kernel is not an agent. It is the thing agents are built on. Different agent harnesses — coding agents, support agents, legal agents, CI agents — are **distributions** that package the kernel with domain-specific tools, policies, and frontends. Like Ubuntu packages the Linux kernel with GNOME and apt, or Android packages it with Dalvik and Play Services.

### What v0.1 must ship

- A working turn loop that calls a model, dispatches tools, and feeds results back
- A context manager with tiered memory and opinionated compaction
- A permission evaluator with policy-file-driven dispatch gating
- A session manager (single-session in v0.1, multi-session interface defined for v0.2)
- Six stable module interfaces: ProviderInterface, ToolRegistration, ChannelInterface, FrontendEvents, SessionControl, PolicyInterface
- Two kernel-internal tools: `request_tool` (demand-paging) and `plan` (scratchpad access)
- Three-path tool loading: native Rust crates, external processes (JSON-RPC over stdin/stdout), MCP bridge
- A reference TUI frontend
- A reference coding-agent distribution (separate package) with ~10 tools
- A policy file format with at least two example policies (permissive, locked-down)

### What v0.1 explicitly defers

- L2 OS sandbox (seccomp-BPF, namespace isolation) — stub the interface, implement in v0.2
- Tool registry / marketplace — tools are local files in v0.1
- Benchmark harness — design is settled, implementation comes after core ships
- Multi-session support — session manager interface defined in v0.1, multi-session in v0.2
- Sub-agent spawning (`spawn_agent` tool) — requires multi-session
- Webhook channels and event routing — requires multi-session
- Web UI and IDE extension frontends

---

## 2. Architecture Overview

```
┌─────────────────────────────────────────────────┐
│                  CHANNELS (data plane)            │
│   WhatsApp · Slack · Webhooks · Cron · TUI input  │
├─────────── ChannelInterface ──────────────────────┤
│                                                   │
│              FRONTENDS (control plane)             │
│        TUI  ·  IDE Extension  ·  Web Dashboard    │
├──── FrontendEvents + SessionControl ────────────┤
│                                                   │
│                    CORE                            │
│  ┌─────────────────────────────────────────────┐ │
│  │          Session Manager (singleton)         │ │
│  │  spawn · wait · route · track · enforce      │ │
│  │                    ↕                          │ │
│  │  ┌─ Session ──────────────────────────────┐  │ │
│  │  │  Turn Loop                             │  │ │
│  │  │  input → prompt → model → dispatch     │  │ │
│  │  │                ↕                        │  │ │
│  │  │  Context Manager (per-session)         │  │ │
│  │  │  token budget · tiered memory          │  │ │
│  │  │                ↕                        │  │ │
│  │  │  Permission Evaluator (per-session)    │  │ │
│  │  │  dispatch gate · policy check          │  │ │
│  │  └────────────────────────────────────────┘  │ │
│  │  (multiple sessions may run concurrently)     │ │
│  └─────────────────────────────────────────────┘ │
│                                                   │
├─── ProviderInterface ──┬── ToolRegistration ─────┤
│       MODULES          │        MODULES           │
│  Anthropic provider    │  FileRead / FileWrite    │
│  OpenAI provider       │  Shell executor          │
│  Ollama provider       │  MCP bridge / Git ops    │
├────────────────────────┴──────────────────────────┤
│              SECURITY (spans all layers)           │
│  L1 Dispatch Gate · L2 OS Sandbox · L3 Budgets    │
└───────────────────────────────────────────────────┘
```

### Core boundary test

The core is defined by a single question: *what cannot be swapped out at runtime without bringing down the system?*

**In the core (monolithic, cannot be replaced):**
- Turn Loop — the scheduler equivalent (instantiable — one per session)
- Context Manager — the memory manager equivalent (one per session)
- Permission Evaluator — the LSM hook equivalent (one per session)
- Session Manager — the process table equivalent (singleton)

**Behind stable interfaces (independently evolvable):**
- Model providers — implement ProviderInterface
- Tool implementations — implement ToolRegistration
- Channels — implement ChannelInterface (webhooks, Slack, cron, file watchers, TUI input)
- UI frontends — implement FrontendEvents and use SessionControl (TUI, IDE extension, web dashboard)
- Policy configurations — implement PolicyInterface

### Why microkernel-influenced, monolithic development

The IPC tax that doomed microkernels in OS design (~100x overhead) is negligible in agent systems (~0.1% of LLM inference latency). We take the microkernel's isolation for free (tools run as separate processes where possible) while taking the monolithic kernel's development discipline (subsystem maintainers, merge windows, regression tracking) for velocity.

---

## 3. The Core

### 3.1 Turn Loop

The turn loop is the heartbeat. Every other subsystem exists to serve it.

```
fn run_turn(input: UserInput, state: &mut SessionState) -> TurnResult {
    // 1. Construct prompt
    let prompt = context_manager.assemble(
        state.system_prompt,
        state.conversation_history,
        state.active_tools,        // demand-paged by context manager
        state.scratchpad,
        input,
    );
    
    // 2. Call model
    let response = provider.complete(prompt, state.config)?;
    
    // 3. Parse tool calls from response
    let tool_calls = parse_tool_calls(response);
    
    // 4. For each tool call: dispatch through permission evaluator
    let mut results = Vec::new();
    for call in tool_calls {
        // L1: Dispatch gate check
        let decision = permission_evaluator.evaluate(
            &call.tool,
            &call.tool.capabilities,
            &state.policy,
        );
        
        match decision {
            Decision::Allow => {
                let result = call.tool.execute(call.input)?;
                
                // Process invalidations before next turn
                for invalidation in &result.invalidations {
                    match invalidation {
                        Invalidation::Files(paths) => {
                            context_manager.invalidate_cached_files(paths);
                        }
                        Invalidation::WorkingDirectory(new_root) => {
                            // Policy check: is the new path within allowed scope?
                            if permission_evaluator.check_path(new_root, &state.policy).is_allow() {
                                state.workspace_root = new_root.clone();
                                context_manager.invalidate_all_cached_files();
                                frontend.on_workspace_changed(new_root);
                            }
                        }
                        Invalidation::ToolRegistry => {
                            context_manager.reload_tool_registry();
                        }
                        Invalidation::Environment(vars) => {
                            context_manager.note_env_change(vars);
                        }
                    }
                }
                
                results.push(result);
            }
            Decision::Deny(reason) => {
                results.push(ToolResult::denied(reason));
            }
            Decision::Ask => {
                let user_decision = frontend.on_permission_request(&call)?;
                // ... handle user response
            }
        }
    }
    
    // 5. Feed results back into context
    context_manager.append_tool_results(results);
    
    // 6. Check if model wants to continue or yield to user
    // (repeat from step 1 if model has more tool calls)
}
```

The turn loop owns the execution flow. It is single-threaded per session. It does not know what model it's talking to (that's behind ProviderInterface) or what tools are available (that's managed by the context manager and tool registry). It does not know what the frontend looks like.

### 3.2 Context Manager

The context manager owns the token budget. It is the only subsystem with global visibility across all context sources.

#### Tiered memory model

```
┌─────────────────────────────────────────┐
│  Tier 1: Working Memory (reserved)       │
│  - Current turn input                    │
│  - Active tool call + result             │
│  - System prompt                         │
│  - Scratchpad (task tracking +           │
│    constraints register)                 │
│  Budget: reserved, never touched         │
├─────────────────────────────────────────┤
│  Tier 2: Short-Term Memory              │
│  - Recent turns (full fidelity)          │
│  - Older turns (progressively            │
│    summarized with retrieval keys)       │
│  - Tool results (ephemeral cache)        │
│  Budget: managed, evictable              │
├─────────────────────────────────────────┤
│  Tier 3: Long-Term Memory               │
│  - Session event stream (append-only    │
│    JSONL, authoritative record)          │
│  - Outside context entirely              │
│  - Accessible via replay/hydration      │
│    and file-based tools                  │
│  Budget: unlimited (disk/DB)             │
└─────────────────────────────────────────┘
```

**Key principle:** Context is not memory — context is attention. Unlike OS memory (passive storage), LLM context actively shapes every token generated. A file at the bottom of 128k context degrades attention to everything else. This reframes compaction from "what to evict" to "what should the model be thinking about right now."

#### Tier-3 event stream (authoritative session record)

Tier 3 is not abstract. Every mutation to the in-memory view — `append_user_input`, `append_assistant_response`, `append_tool_exchange`, `append_system_message`, plus a once-per-session `record_session_started` — fans out through a `SessionEventSink` trait to an append-only `SessionEvent` stream before the view itself is touched. The stream is the authoritative session record; the `ContextStore` is a derivable projection of it. The record-before-mutate ordering makes "the stream is the truth" a structural invariant: a crash between record and mutate leaves the stream slightly ahead, which is recoverable; the opposite ordering is not.

Four sinks ship in `kernel-core`: `NullSink` (drops every event — the default for unit tests and in-process library callers), `FileSink` (appends one JSON object per line to a specified path, `u64` millisecond timestamps, serde-tagged variants), `HttpSink` (POSTs each event as a single JSON object to `<endpoint>/events` over a hand-rolled `std::net::TcpStream` HTTP/1.1 client — no dependencies, `http://` only, optional bearer token, 2-second timeout, audit-only fire-and-forget with a `failed_writes` counter), and `TeeSink<A, B>` (a generic composite that fans `record()` out to two inner sinks). `SessionEventSink` is also implemented for `Box<dyn SessionEventSink>`, so `TeeSink` can hold a runtime-selected primary sink alongside a concrete secondary. `ContextManager` exposes `new`, `with_store`, `with_event_sink`, and `with_store_and_events` constructors so test and production call sites can mix-and-match store and sink independently. `FileSink` and `HttpSink` both expose `failed_writes()` for observability — a non-zero counter means the stream is no longer authoritative for that sink.

The `kernel-daemon` router wires a `FileSink` for every session it creates, resolving the path via `session_events::default_events_path(session_id)`. The base directory is `$AGENT_KERNEL_HOME` if set, else `$HOME/.agent-kernel`, else `./.agent-kernel`; the events file lives at `<base>/sessions/{id}/events.jsonl`. Sessions are globally addressable — the workspace is recorded inside the `SessionStarted` event as metadata, not encoded in the filesystem path, so any past session's events can be located without knowing the original workspace. If `FileSink::new` fails (unwritable base directory) the daemon falls back to `NullSink` with a stderr warning rather than aborting session creation. If `AGENT_KERNEL_REMOTE_SINK_URL` is set, the router additionally constructs an `HttpSink` (with optional `AGENT_KERNEL_REMOTE_SINK_TOKEN` bearer auth) and wraps both sinks in a `TeeSink` so every event is mirrored to the remote endpoint for one-way audit/archival; a malformed URL logs to stderr and falls back to local-only, and remote delivery failures are counted but never block the turn loop. True remote session execution and remote hydration are out of scope here — the kernel still runs locally.

**Replay and hydration.** The read side lives in the same module. `session_events::read_events_from_file(path)` parses a JSONL file line-by-line into `Vec<SessionEvent>`, failing with `io::ErrorKind::InvalidData` and the 1-based line number on the first malformed entry — no forward recovery. `ContextManager::replay_events(&[SessionEvent])` walks an event slice and calls the same `append_*` methods that wrote them (against a `NullSink` so replay doesn't duplicate events back out to disk); `ContextManager::hydrated_from_events(config, events)` is the constructor form, which requires `SessionStarted` as the first event to recover the original `system_prompt`. `SessionManager::hydrate_from_events` builds a full `Session` around a hydrated `ContextManager` and registers it — policy, tools, completion config, and resource budget are caller-provided because they aren't round-trippable through the current event schema. Workspace sync and cross-machine hydration are deferred.

#### The information asymmetry problem

The context manager knows what fits but not what matters. The model knows what matters but not what fits. Two mechanisms address this:

1. **Observe model behavior** (default) — like Linux's accessed bit. If the model references a tool or piece of context, it's important. If it doesn't, it's a candidate for eviction.

2. **Model annotations** (optional) — like `madvise()`. The model can provide advisory hints about what's important. These are advisory, not mandatory.

#### Opinionated compaction

Compaction is internal to the core and not a swappable strategy. It touches every internal data structure the context manager owns. Making it pluggable creates an interface so wide it's not really a boundary (like Linux's page replacement — compiled in, not a loadable module).

Compaction is a **projection over the in-memory view**, not a mutation of history. `ContextManager::compact(&dyn ProviderInterface)` walks compacted turns, formats each as prose via an internal `turn_to_prose` helper, and calls `provider.complete(...)` with a hard-coded 2-3-sentence summarization prompt (concise compaction assistant, preserve concrete facts, drop incidental detail, no tool definitions) to generate a real summary. The old 100-char `summarize_turn()` truncation stub is gone. The Tier-3 event stream is untouched by compaction — this is pinned by a `compaction_does_not_touch_event_stream` test that compares real `FileSink` bytes before and after — so the full-fidelity history can always be re-derived by replaying events into a fresh `ContextManager`. `SessionControl::request_compaction` takes a `&dyn ProviderInterface` argument for the same reason; the `RequestCompaction` event-loop handler passes `&*self.provider` through.

**Compaction defaults:**

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| Trigger threshold | 60-70% capacity | Not 80-95% like Claude Code/OpenCode. Earlier compaction preserves more signal. |
| Scratchpad | Persists to disk, survives compaction | Task tracking and constraints register must never be lost |
| File contents | Ephemeral cache — evict freely | Can always be re-read from disk |
| User intent/constraints | Pinned — never evict | "Don't modify the auth module" must survive any compaction |
| Verbatim tail | Last ~30% of conversation kept uncompressed | Recent context is highest-value |
| Post-compaction hooks | First-class citizens | Frontends need to know compaction happened |

**Death spiral guards:**

- Cooldown timer: minimum interval between compactions (prevent rapid-fire)
- Circuit breaker: after 3 compaction failures in sequence, halt and surface error to user
- System prompt budget cap: system prompt cannot exceed N% of total context (prevents the OpenCode AGENTS.md problem where 331KB consumed 81% of 128K window)

#### Demand-paging via `request_tool`

`request_tool` is an always-loaded meta-tool. When the model needs a capability it doesn't currently have access to, it calls `request_tool`:

```json
{
  "tool": "request_tool",
  "input": {
    "description": "I need a tool that can interact with GitHub issues"
  }
}
```

The context manager searches the full tool registry, matches on relevance signals, evaluates token cost against remaining budget, and pages in the best match. If budget doesn't allow it, the context manager may evict a less-relevant loaded tool first.

This is analogous to Linux's `request_module()` / `modprobe`. It appears in zero surveyed competitor projects.

### 3.3 Permission Evaluator

The dispatch gate. Intercepts every tool invocation before execution. Provides **mechanism only** — policy is external configuration.

```
fn evaluate(tool: &Tool, capabilities: &Set<Capability>, policy: &Policy) -> Decision {
    // Check each capability the tool declares against the loaded policy
    for cap in capabilities {
        match policy.check(cap, context) {
            PolicyResult::Allow => continue,
            PolicyResult::Deny(reason) => return Decision::Deny(reason),
            PolicyResult::RequireApproval => return Decision::Ask,
        }
    }
    Decision::Allow
}
```

The permission evaluator must be in the core because if it were a module, it could be unloaded or bypassed. This is the same reason LSM hooks are compiled into the Linux kernel, not loadable modules.

### 3.4 Session Manager

The process table. A singleton that owns all running sessions and is the single entry point for session creation regardless of trigger source.

```rust
struct SessionManager {
    sessions: HashMap<SessionId, Session>,
    global_budget: ResourceBudget,        // total across all sessions
    routing_table: RoutingTable,          // webhook → session config mapping
}

struct Session {
    id: SessionId,
    turn_loop: TurnLoop,                  // its own turn loop instance
    context_manager: ContextManager,      // its own context window
    permission_evaluator: PermissionEvaluator, // its own policy
    tools: Vec<Box<dyn ToolRegistration>>,
    frontend: Box<dyn FrontendEvents>,
    mode: SessionMode,                    // interactive or autonomous
    resource_budget: ResourceBudget,      // carved from global budget
    workspace: PathBuf,
    parent: Option<SessionId>,            // if spawned by another session
    pending_results: Vec<PendingResult>,  // from children or webhook notifications
}
```

**Three spawn triggers — all go through the same path:**

```rust
impl SessionManager {
    /// Trigger 1: Human starts a conversation
    fn spawn_interactive(&mut self, frontend: impl FrontendEvents, config: SessionConfig) -> SessionId;
    
    /// Trigger 2: Running session spawns a child (via spawn_agent tool)
    fn spawn_child(&mut self, parent: SessionId, config: ChildSessionConfig) -> SessionId;
    
    /// Trigger 3: External event arrives (via channel module)
    fn spawn_from_event(&mut self, event: ExternalEvent) -> Option<SessionId>;
    
    /// Wait for a child session to complete (blocking)
    fn wait(&self, session_id: SessionId) -> SessionResult;
    
    /// Deliver a notification to a session's pending_results queue
    fn notify(&mut self, session_id: SessionId, result: PendingResult);
    
    /// Route invalidations from one session to others with overlapping cached state
    fn propagate_invalidation(&mut self, source: SessionId, invalidation: &Invalidation);
}
```

**Webhook/event delivery has three modes, configured in the routing table:**

```yaml
# routing.yaml — how channel events map to sessions
routes:
  # Mode 1: spawn — always create a new session (safest)
  - match: { source: "github", event: "pull_request.opened" }
    delivery: spawn
    agent: "pr-reviewer"
    policy: "ci-lockdown.yaml"
    tools: ["file_read", "grep", "git"]
    budget: { max_tokens: 200_000 }

  # Mode 2: notify — deliver to a matching open session's pending queue
  # The model sees it as a system message between turns, never mid-turn
  - match: { source: "github-ci", event: "check_run.failed" }
    delivery: notify_matching_session
    match_session: { workspace_branch: "{event.branch}" }
    fallback: spawn                    # if no matching session, create one
    agent: "ci-analyzer"
    policy: "ci-lockdown.yaml"

  # Mode 3: drop — ignore if no matching session exists
  - match: { source: "slack", event: "reaction_added" }
    delivery: notify_matching_session
    match_session: { workspace: "{event.channel}" }
    fallback: drop                     # not worth spawning a session for
```

**The safety invariant:** events and child results are delivered to `pending_results` and drained by the turn loop **between turns, never mid-turn**. The turn loop is the critical section. Nothing external modifies the context while a turn is executing.

```rust
// At the top of every turn, the turn loop drains pending results:
fn run_turn(&mut self) {
    for result in self.pending_results.drain(..) {
        match result {
            PendingResult::ChildCompleted { task, message, invalidations } => {
                self.context_manager.append_system_message(
                    format!("Background agent '{}' completed: {}", task, message)
                );
                for inv in invalidations {
                    self.context_manager.process_invalidation(inv);
                }
            }
            PendingResult::ExternalEvent { source, event_type, summary } => {
                self.context_manager.append_system_message(
                    format!("[{}] {}: {}", source, event_type, summary)
                );
            }
        }
    }
    
    // Now construct prompt — all pending results are visible to the model
    let prompt = self.context_manager.assemble(...);
    // ... rest of turn loop
}
```

**For v0.1:** the session manager manages exactly one session. The interface exists so that v0.2 can add multi-session, child spawning, and webhook delivery without breaking the core architecture.

**Shipped session-construction entry points:** `spawn_interactive` (default `NullSink`), `spawn_interactive_with_events` (caller supplies a `Box<dyn SessionEventSink>`, used by the daemon and by event-stream integration tests), and `hydrate_from_events` (reads an `events.jsonl` file and reconstructs an in-memory session via `ContextManager::hydrated_from_events`). `spawn_child` and `spawn_from_event` remain interface-only until multi-session lands.

---

## 4. Module Interfaces

These are the `EXPORT_SYMBOL` boundary — the stable contracts between core and modules. Internal implementation (prompt engineering, context packing strategy, compaction algorithm) can change freely between versions. These interfaces carry backward-compatibility guarantees.

### 4.1 ProviderInterface

```rust
trait ProviderInterface {
    /// Blocking completion
    fn complete(prompt: Prompt, config: CompletionConfig) -> Result<Response, ProviderError>;
    
    /// Streaming completion
    fn stream(prompt: Prompt, config: CompletionConfig) -> Result<Stream<Chunk>, ProviderError>;
    
    /// Token counting for budget management
    fn count_tokens(content: &Content) -> usize;
    
    /// What this provider supports (tool use, vision, streaming, etc.)
    fn capabilities() -> ProviderCaps;
}
```

The turn loop calls these methods. It never touches provider-specific APIs. A provider module for Anthropic, OpenAI, Google, or Ollama all implement this same interface.

### 4.2 ToolRegistration

The chokepoint contract. Every tool — built-in, MCP bridge, or user-defined — registers against this interface.

```rust
struct ToolRegistration {
    /// What system resources this tool touches.
    /// The dispatch gate checks these against loaded policy.
    /// Examples: fs:read, fs:write, net:api.github.com, shell:exec, env:read
    capabilities: Set<Capability>,
    
    /// Input/output contract for the model.
    /// JSON Schema with parameter types, descriptions, required fields.
    /// Injected into the prompt when the tool is active.
    schema: ToolSchema,
    
    /// Approximate tokens consumed when this tool's schema is in context.
    /// The context manager uses this to budget the active tool set.
    cost: TokenEstimate,
    
    /// When this tool should be loaded into context.
    /// Tags, keyword triggers, or predicates.
    /// The context manager uses this for demand-paging decisions.
    relevance: RelevanceSignal,
    
    /// The actual work.
    fn execute(input: ToolInput) -> Result<ToolOutput, ToolError>;
}

/// What a tool returns after execution.
/// Every tool result includes an optional set of invalidations
/// that tell the kernel what cached state is now stale.
struct ToolOutput {
    /// The result the model sees
    result: Value,
    
    /// What this tool's execution invalidated.
    /// The context manager processes these before the next turn.
    /// Default: empty (read-only tools invalidate nothing).
    invalidations: Vec<Invalidation>,
}

enum Invalidation {
    /// These cached file contents are stale — re-read before using.
    /// Produced by: file_write, file_edit, git_merge, git_stash_pop
    Files(Vec<PathBuf>),
    
    /// The workspace root has changed — all relative paths are stale.
    /// Produced by: git_worktree switch, cd
    WorkingDirectory(PathBuf),
    
    /// The set of available tools has changed — re-scan the registry.
    /// Produced by: mcp_connect, mcp_disconnect, tool_install
    ToolRegistry,
    
    /// These environment variables changed — tools depending on them may behave differently.
    /// Produced by: env_set, nvm use, pyenv shell
    Environment(Vec<String>),
}
```

**Why four metadata fields matter:**

| Field | Consumed by | Purpose |
|-------|-------------|---------|
| `capabilities` | Permission Evaluator | Gate execution against policy |
| `schema` | Context Manager → Model | Tell the model how to call this tool |
| `cost` | Context Manager | Budget which tools fit in context |
| `relevance` | Context Manager | Decide when to page this tool in |

MCP tools register through a bridge module that translates MCP's tool definitions into this format. The bridge adds capability declarations (inferred from MCP's `readOnlyHint`, `destructiveHint` annotations where available), estimates cost from schema size, and generates relevance signals from tool names and descriptions. The core never knows whether a tool is built-in or MCP-backed.

#### Invalidation protocol

Tools that modify the environment the kernel operates in must signal those changes back through `ToolOutput.invalidations`. This is a fundamental part of the tool protocol, not a special case — almost every write operation potentially invalidates something the kernel is caching.

**How the context manager processes invalidations:**

| Invalidation | Context manager action |
|-------------|----------------------|
| `Files(paths)` | Marks cached file contents in Tier 2 as stale. Next access re-reads from disk. |
| `WorkingDirectory(path)` | Updates workspace root. Invalidates all cached file contents. Notifies frontend. Subject to policy check — the new path must be within allowed scope. |
| `ToolRegistry` | Re-scans tool manifests and MCP servers. Updates the demand-paging registry. May page in newly available tools on next turn. |
| `Environment(vars)` | Records the change for observability. Does not invalidate tools directly, but the scratchpad notes the change so the model is aware. |

**Default behavior:** Read-only tools return empty invalidations. The built-in `file_write` and `file_edit` tools automatically produce `Files` invalidations for every path they modify. External tools declare invalidations in their JSON-RPC response:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "data": {"branch": "auth-refactor", "path": "../project-auth-refactor"},
    "invalidations": [
      {"type": "working_directory", "path": "../project-auth-refactor"}
    ]
  }
}
```

External tools that omit the `invalidations` field produce no invalidations — the kernel assumes nothing changed. This is safe for read-only tools but means a write tool that forgets to declare invalidations will cause the context manager to serve stale cached data. This is a correctness bug in the tool, not the kernel — analogous to a filesystem driver that doesn't mark pages dirty after a write.

#### Three-path tool implementation model

All tools — regardless of implementation language or execution model — register against the same `ToolRegistration` interface. The kernel provides three paths:

**Path 1: Native Rust crates (in-process, fast path).** Tools compiled as Rust crates linked into the kernel binary at build time via Cargo features. Zero IPC overhead. Used for performance-critical tools called hundreds of times per session. Distribution authors opt into these via `Cargo.toml` feature flags — the kernel binary itself ships no domain tools.

**Path 2: External processes (JSON-RPC over stdin/stdout, any language).** Tools running as standalone executables. The kernel spawns the process, sends JSON-RPC requests on stdin, reads responses from stdout. ~5ms IPC overhead per invocation. This is the ecosystem growth path — tool authors write Python, TypeScript, Go, or any language that can read/write JSON.

**Path 3: MCP bridge (protocol translation).** MCP servers register through a bridge module that translates MCP tool definitions into `ToolRegistration` format. The bridge infers capability declarations from MCP's `readOnlyHint`/`destructiveHint` annotations, estimates cost from schema size, and generates relevance signals from tool names and descriptions. Each MCP server's tools appear as individual `ToolRegistration` entries.

All three paths produce `Box<dyn ToolRegistration>`. The kernel, context manager, and permission evaluator treat them identically. The only difference is execution latency.

```
fn load_tools(config: &Config) -> Vec<Box<dyn ToolRegistration>> {
    let mut tools = Vec::new();
    
    // Path 1: Native Rust tools (compiled in via Cargo features)
    #[cfg(feature = "tool-filesystem")]
    tools.extend(tool_filesystem::register());
    
    // Path 2: External tools from tool.toml manifests
    for manifest in discover_tool_manifests(&config.tools.external_dirs) {
        tools.push(Box::new(ExternalTool::from_manifest(manifest)));
    }
    
    // Path 3: MCP servers
    for (name, mcp_config) in &config.tools.mcp {
        let bridge = McpBridge::connect(name, mcp_config)?;
        for mcp_tool in bridge.list_tools()? {
            tools.push(Box::new(McpToolAdapter::from(mcp_tool)));
        }
    }
    
    tools  // all three paths produce the same type
}
```

#### Tool SDK

The kernel ships lightweight SDKs for Python, TypeScript, and Go that eliminate JSON-RPC boilerplate. A tool author writes a decorated function — the SDK generates the JSON-RPC loop, JSON Schema from type hints, and tool.toml manifest from decorator metadata.

**Python:**
```python
from agent_kernel import tool, Invalidation

@tool(
    capabilities=["fs:read", "fs:write", "shell:exec"],
    relevance=["branch", "worktree", "parallel", "stash"],
    cost=320,
)
def git_worktree(action: str, branch: str = None, path: str = None):
    """Manage git worktrees for parallel branch work."""
    if action == "create":
        # ... implementation
        return {"path": path, "branch": branch}
    elif action == "switch":
        resolved = resolve_worktree(path, branch)
        return (
            {"switched_to": resolved},
            [Invalidation.working_directory(resolved)]
        )
```

**What the SDK does:** reads the function signature and docstring → generates JSON Schema for `parameters` → wraps the function in a JSON-RPC stdin/stdout loop → handles error serialization and invalidation formatting → generates `tool.toml` at publish time via `agent-kernel publish`.

The SDK is a convenience, not a requirement. Any executable that reads JSON-RPC from stdin and writes JSON-RPC to stdout is a valid tool. The SDK just makes the common case trivial.

### 4.3 FrontendEvents + SessionControl

The frontend abstraction is split into two traits: **FrontendEvents** for notifications flowing out of the kernel, and **SessionControl** for commands flowing in. The core never knows whether it's talking to a TUI, an IDE extension, or a web dashboard.

```rust
/// Event notifications from the kernel to the frontend.
trait FrontendEvents: Send {
    /// A new turn is starting
    fn on_turn_start(&self, turn_id: TurnId);
    
    /// Streaming chunk from the model
    fn on_stream_chunk(&self, chunk: &StreamChunk);
    
    /// The model produced text output (non-streaming path)
    fn on_text(&self, text: &str);
    
    /// A tool is being called (for display)
    fn on_tool_call(&self, tool_name: &str, input: &serde_json::Value);
    
    /// A tool produced a result
    fn on_tool_result(&self, tool_name: &str, result: &ToolOutput);
    
    /// Permission required — returns user's decision
    fn on_permission_request(&self, request: &PermissionRequest) -> Decision;
    
    /// The turn is complete
    fn on_turn_end(&self, turn_id: TurnId);
    
    /// Context was compacted (post-compaction hook)
    fn on_compaction(&self, summary: &CompactionSummary);
    
    /// The workspace root changed (e.g. worktree switch)
    fn on_workspace_changed(&self, new_root: &Path);
    
    /// Error occurred
    fn on_error(&self, error: &KernelError);
}

/// The command surface for frontends to control a running session.
trait SessionControl: Send {
    // --- Queries ---
    fn tokens_used(&self) -> usize;
    fn context_utilization(&self) -> f64;
    fn turn_count(&self) -> usize;

    // --- Commands ---
    /// Signal cancellation — the turn loop stops dispatching tools.
    fn cancel(&self);
    /// Force context compaction. Returns tokens freed. Takes a provider
    /// because compaction calls the model to generate real summaries.
    fn request_compaction(&mut self, provider: &dyn ProviderInterface) -> Result<usize, String>;
    /// Hot-swap the active policy.
    fn set_policy(&mut self, policy: Policy);
}
```

For a CI/headless frontend, `on_permission_request` always returns the policy's default (allow or deny, never ask). For a TUI, it renders an interactive prompt.

### 4.4 PolicyInterface

External policy configuration. Mechanism (core) and policy (config files) are fully separated. Same binary, different policy file = developer laptop vs enterprise CI pipeline.

```yaml
# policy.yaml — example permissive policy for solo developer
version: 1
name: "developer-permissive"

rules:
  - match: { capabilities: ["fs:read"] }
    action: allow
    
  - match: { capabilities: ["fs:write"] }
    action: allow
    scope: { paths: ["{workspace}/**"] }
    
  - match: { capabilities: ["shell:exec"] }
    action: ask
    
  - match: { capabilities: ["net:*"] }
    action: ask
    
  - match: { capabilities: ["env:read"] }
    action: deny
    except: ["PATH", "HOME", "SHELL"]

resource_budgets:
  max_tokens_per_session: 1_000_000
  max_tool_invocations_per_turn: 20
  max_wall_time_per_tool: 120s
  max_output_size_per_tool: 100KB
```

```yaml
# policy.yaml — example locked-down policy for CI pipeline
version: 1
name: "ci-lockdown"

rules:
  - match: { capabilities: ["fs:read"] }
    action: allow
    scope: { paths: ["{workspace}/**"] }
    
  - match: { capabilities: ["fs:write"] }
    action: allow
    scope: { paths: ["{workspace}/**"] }
    exclude: [".env", "*.key", "*.pem"]
    
  - match: { capabilities: ["shell:exec"] }
    action: allow
    scope: { commands: ["git", "npm", "pytest", "cargo"] }
    
  - match: { capabilities: ["net:*"] }
    action: deny
    
  - match: { capabilities: ["env:read"] }
    action: deny

resource_budgets:
  max_tokens_per_session: 500_000
  max_tool_invocations_per_turn: 10
  max_wall_time_per_tool: 60s
  max_output_size_per_tool: 50KB

audit:
  log_all_decisions: true
  log_destination: stdout
```

Policy files are version-controlled, auditable, and shareable. They are the first ecosystem flywheel — enterprise teams publish and iterate on policies the way they share SELinux profiles.

### 4.5 ChannelInterface

How external events enter the kernel. Channels are pluggable modules that accept inbound connections (HTTP webhooks, messaging platforms, cron schedules, file system watchers) and produce `ExternalEvent` values that the session manager routes.

The terminology comes from OpenClaw, which uses "channels" to describe connections to WhatsApp, Slack, Telegram, and other messaging platforms. We generalize the concept: a channel is any push-based event source that can trigger agent sessions.

```rust
trait ChannelInterface {
    /// Start listening. Calls event_sink when events arrive.
    /// The session manager handles routing — the channel just produces events.
    fn start(&self, event_sink: impl Fn(ExternalEvent));
    
    /// Clean shutdown
    fn stop(&self);
    
    /// What event types this channel can produce
    fn produces(&self) -> Vec<String>;
    
    /// Capabilities this channel requires (e.g., net:listen:8080)
    fn capabilities(&self) -> Set<Capability>;
}

struct ExternalEvent {
    source: String,         // "github-webhook", "slack", "cron", "file-watcher"
    event_type: String,     // "pull_request.opened", "message", "daily", "file.changed"
    payload: Value,         // raw event data
}
```

Channels are pluggable the same way tools are — a TOML manifest plus an executable speaking JSON-RPC over stdin/stdout:

```toml
# channels/github-webhook/channel.toml
[channel]
name = "github_webhook"
version = "0.1.0"
command = "python3"
args = ["github_listener.py"]
type = "long-running"

[channel.capabilities]
net = ["listen:8080"]

[channel.produces]
events = ["pull_request.opened", "pull_request.merged", "check_run.failed"]
```

#### Channels vs Frontends

This is a deliberate architectural distinction:

**Channels** are the data plane — event pipes. A WhatsApp channel receives messages and sends responses. It doesn't let you inspect sessions, view token budgets, switch between sessions, or manage policies. It creates sessions and delivers events to them. Input in, output out.

**Frontends** are the control plane — rich interfaces for humans to operate the kernel. A TUI lets you see all active sessions, switch between them, inspect context usage, view audit logs, configure policies, promote an autonomous session to interactive, kill runaway sessions. A VS Code extension shows inline tool results, diffs, and permission prompts in the editor.

A channel can *create* sessions and *deliver events* to them. A frontend can *observe, manage, and control* sessions.

The TUI plays both roles — it's a frontend (control plane for managing the system) *and* a channel (event source feeding human input into sessions). But those are two separate interfaces it implements, not one unified concept.

Some channels are interactive (WhatsApp, Slack — the user can send follow-up messages and answer permission prompts through the channel). Some are unidirectional (webhooks, cron — fire and forget). The channel type determines whether the session it creates is `Interactive` or `Autonomous`:

```rust
enum SessionMode {
    /// Human attached — can send follow-up input, answer permission prompts,
    /// cancel operations, provide clarification mid-task.
    Interactive { frontend: Box<dyn FrontendEvents> },
    
    /// No human in the loop. All decisions from policy.
    /// Permission::Ask → Permission::Deny.
    Autonomous { output_sink: OutputSink },
}
```

Autonomous sessions can be **promoted** to interactive. When a webhook-spawned agent hits a policy limit it can't resolve, the session enters `WaitingForPermission` state. The session manager surfaces it to the frontend: "Session B needs attention." The human attaches, grants permission, and detaches. The session returns to autonomous mode.

**For v0.1:** no channel modules ship. The TUI acts as both frontend and channel for a single interactive session. The `ChannelInterface` exists so that v0.2 can add webhook support, Slack adapters, and cron scheduling without modifying the core.

---

## 5. Security Architecture

Three layers, operating independently. Compromising one doesn't compromise the others.

### L1: Dispatch Gate (before execution)

The permission evaluator checks the tool's declared capabilities against loaded policy. Fast, cheap, provides good error messages. Trusts the tool's self-description.

**Enforces:** Tool capabilities vs policy rules. Allow / deny / ask-user.

**Linux analogy:** Permission checks on `open()` — evaluates intent before action.

### L2: OS Sandbox (during execution) — *v0.1: stub interface only*

All tool execution runs inside an OS-level sandbox. Filesystem path filtering, network endpoint filtering, syscall filtering via seccomp-BPF. Enforces based on actual behavior, not declared intent.

**Enforces:** Syscalls, file paths, network endpoints. Kill on violation.

**Linux analogy:** seccomp-BPF + namespaces.

*v0.1 ships the interface definition and a no-op passthrough implementation. v0.2 implements seccomp-BPF on Linux and Seatbelt on macOS.*

### L3: Resource Budgets (continuous)

Token budgets per session, compute time limits per tool invocation, memory limits per process, output size limits. On exhaustion: deterministic action (terminate the tool call cleanly, not the entire session).

**Enforces:** Tokens, wall time, memory, output size. Graceful termination on breach.

**Linux analogy:** cgroups — limits what a process can consume, kills the cgroup as a unit.

### Mechanism vs Policy

All three layers provide **mechanism**. What paths are allowed, what endpoints are reachable, what budgets apply — that's **policy**, expressed as external configuration files. Same binary, different policy file = developer laptop vs enterprise CI pipeline.

---

## 6. Kernel Tools vs Distribution Tools

The kernel ships exactly **two built-in tools**. Everything else is a distribution concern.

### Kernel-internal tools (compiled into the kernel, always available)

| Tool | Capabilities | Description |
|------|-------------|-------------|
| `request_tool` | *(none — internal)* | Meta-tool for demand-paging. Model says "I need a tool that can do X," context manager searches registry, pages in the best match. Must be always-loaded because it's the mechanism for loading other tools. |
| `plan` | *(none — internal)* | Read/write the session scratchpad (Tier 1 working memory). Create step-by-step plans, mark steps complete, track progress. Survives compaction. Must be kernel-internal because the scratchpad is an internal data structure the context manager owns. |

These tools have empty capability sets — they don't touch external resources. The permission evaluator always allows them. The context manager always loads them.

### Distribution tools (provided by distributions, not the kernel)

The reference `dist-code-agent` distribution ships with these tools:

| Tool | Capabilities | Path | Description |
|------|-------------|------|-------------|
| `file_read` | `fs:read` | Native Rust | Read file contents with line ranges |
| `file_write` | `fs:write` | Native Rust | Write full file contents |
| `file_edit` | `fs:read, fs:write` | Native Rust | String replacement editing |
| `grep` | `fs:read` | Native Rust | Search file contents (wraps ripgrep) |
| `glob` | `fs:read` | Native Rust | Find files by pattern |
| `ls` | `fs:read` | Native Rust | List directory contents |
| `shell` | `shell:exec` | Native Rust | Execute shell commands with timeout |
| `git` | `fs:read, fs:write, shell:exec` | Native Rust | Git operations |
| `web_fetch` | `net:*` | Native Rust | Fetch URL contents (isolated context for security) |

A different distribution — `dist-support-agent` — would ship a completely different tool set: ticket management, knowledge base search, escalation routing. The kernel doesn't assume what you're building.

### Three-path tool implementation

All tools — kernel-internal, distribution, or third-party — produce the same `Box<dyn ToolRegistration>`. The kernel doesn't know or care which path produced each tool.

**Path 1: Native Rust crate (in-process, fast path).** Used for performance-critical tools called hundreds of times per session (file_read, grep). Compiled into the distribution binary via Cargo features. Zero IPC overhead — direct function calls.

```toml
# Distribution's Cargo.toml
[dependencies]
agent-kernel = { version = "0.1", features = ["tool-filesystem", "tool-shell"] }
```

**Path 2: External process (JSON-RPC over stdin/stdout).** The ecosystem growth path. Tool author writes a script in any language, ships it with a `tool.toml` manifest. The kernel spawns the process, sends JSON-RPC requests on stdin, reads responses from stdout. ~5ms IPC overhead per invocation.

```
tools/jira-tool/
├── tool.toml        # manifest: capabilities, schema, cost, relevance
└── jira_tool.py     # implementation: reads JSON from stdin, writes JSON to stdout
```

**Path 3: MCP bridge.** Translates MCP server tools into `ToolRegistration` format. The bridge module discovers tools via `listTools()`, infers capabilities from MCP annotations (`readOnlyHint`, `destructiveHint`), estimates cost from schema size, and generates relevance signals from names and descriptions. Each MCP server's tools register individually. Timeouts, retries, and error propagation are the bridge's responsibility — no silent failures reach the core.

### Tool SDK (eliminates boilerplate)

The kernel ships lightweight SDKs for Python, TypeScript, and Go. The SDK handles JSON-RPC loop, JSON Schema generation from type hints, error wrapping, and manifest generation. A tool author writes a decorated function, not a JSON-RPC server:

```python
from agent_kernel import tool, Invalidation

@tool(
    capabilities=["fs:read", "fs:write", "shell:exec"],
    relevance=["branch", "worktree", "parallel", "stash"],
    cost=320,
)
def git_worktree(action: str, branch: str = None, path: str = None, base: str = "HEAD"):
    """Manage git worktrees for parallel branch work."""
    if action == "create":
        git("worktree", "add", "-b", branch, path or default_path(branch), base)
        return {"path": path, "branch": branch}
    elif action == "switch":
        resolved = resolve_worktree(path, branch)
        return (
            {"switched_to": resolved},
            [Invalidation.working_directory(resolved)]
        )
```

The `@tool` decorator generates the `tool.toml` manifest at publish time from the decorator arguments and function type hints. The SDK is a single file per language with zero dependencies. The tool author never writes JSON-RPC, JSON Schema, or TOML manifests by hand.

---

## 7. Distributions

A distribution is a **manifest of manifests** — tools + policy + skills + provider config + frontend. It packages the kernel with everything needed for a specific agent use case. The relationship is Linux kernel to Ubuntu/Fedora/Android.

```toml
# dist/code-agent.toml

[distribution]
name = "code-agent"
version = "0.1.0"

[provider]
type = "anthropic"
model = "claude-sonnet-4-20250514"

[policy]
file = "policies/developer-permissive.yaml"

[tools.builtin]
# Native Rust tools compiled into the binary via Cargo features
include = ["file_read", "file_write", "file_edit", "grep", "glob", "ls", "shell", "git"]

[tools.external]
# Tools from the registry (loaded from manifests at runtime)
"@acme/jira-tool" = "^0.2.0"

[tools.mcp]
# MCP servers
github = { command = "npx", args = ["-y", "@modelcontextprotocol/server-github"] }

[tools.custom]
# Tools the distro author wrote, shipped inside the distribution
include = ["./tools/custom-linter/"]

[skills]
include = ["python-conventions", "small-commits"]

[frontend]
type = "tui"
```

**Same distribution, different environments:** The distribution manifest stays the same. The *policy file* is what changes the security posture. A `--policy` flag at install time overrides the distribution's default:

```bash
# Developer laptop — uses the distribution's default permissive policy
agent-kernel install dist/code-agent.toml

# CI pipeline — locked-down policy, headless frontend
agent-kernel install dist/code-agent.toml \
  --policy policies/ci-lockdown.yaml \
  --frontend headless

# Enterprise — approved tool catalog, mandatory audit
agent-kernel install dist/code-agent.toml \
  --policy policies/enterprise-soc2.yaml \
  --tool-catalog https://internal.corp/approved-tools
```

Distribution authors never touch Rust. They write Python/TS tools, YAML policies, markdown skills, and a TOML distribution manifest.

---

## 8. Skills Layer

Skills are **prompt-level instructions** that inform the model's behavior. They are not tools and do not participate in the security model, context budgeting, or dispatch system.

```markdown
# skill: python-conventions
When editing Python files:
- Use ruff for formatting
- Prefer functional patterns over classes
- Always add type hints to function signatures
- Run pytest after making changes
```

Skills are loaded into the system prompt. They consume tokens but have no capability declarations — they are advice to the model, not contracts with the kernel. The distinction: skills inform the model, tools inform the kernel.

Skills can be:
- Bundled with a distribution (opinionated defaults)
- Project-level (`.agent-kernel/skills/`)
- User-level (`~/.config/agent-kernel/skills/`)

---

## 9. Configuration

```toml
# agent-kernel.toml

[kernel]
# Context management
context_window = 200_000          # model's total context
compaction_threshold = 0.65       # trigger compaction at 65%
system_prompt_budget = 0.15       # max 15% for system prompt
verbatim_tail_ratio = 0.30        # keep last 30% uncompressed

[provider]
type = "anthropic"
model = "claude-sonnet-4-20250514"
# Provider-specific config goes here

[policy]
file = "policy.yaml"

[tools]
# External tools loaded from manifests at runtime
external_dirs = [".agent-kernel/tools/", "~/.config/agent-kernel/tools/"]

# MCP servers
[tools.mcp.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_TOKEN = "{env:GITHUB_TOKEN}" }

[frontend]
type = "tui"

# Channels (v0.2+) — push-based event sources
# [channels.github-webhook]
# type = "http"
# port = 8080
# path = "/webhooks/github"
#
# [channels.cron]
# schedule = "0 9 * * MON-FRI"   # weekday mornings
# agent = "daily-standup"

[skills]
paths = [".agent-kernel/skills/", "~/.config/agent-kernel/skills/"]
```

---

## 10. Governance Model

Architecture without governance is a diagram. These rules make the architecture survivable.

### Subsystem maintainers

Every directory maps to a named maintainer via a machine-readable `MAINTAINERS` file. PRs touching the security subsystem require security maintainer sign-off. Cross-subsystem changes require all affected owners.

### Time-based releases

Fixed cadence: 2-week feature window, followed by 4-week stabilization (bug fixes only). Features that miss the window wait for the next cycle. No exceptions.

### No-regressions rule

Anything that worked in the previous release and works worse in the new release is a regression. Default resolution: revert the offending change, even if it contains important improvements. Automated regression tracking via bot.

### Accountability tags

Every commit carries metadata:
- `Signed-off-by:` — legal/authorship
- `Reviewed-by:` — technical review
- `Tested-by:` — verification
- `Fixes:` — links to the commit that introduced a bug (enables automated backporting)

### Stable interface guarantee

The six module interfaces (ProviderInterface, ToolRegistration, ChannelInterface, FrontendEvents, SessionControl, PolicyInterface) are versioned and follow a three-stage maturity model inspired by Kubernetes API lifecycle:

**Experimental** — opt-in only (behind a feature flag), may change or disappear without notice, not available in stable builds. New interface additions start here.

**Provisional** — available in stable builds, must either graduate to stable or be replaced within 6 months (3 release cycles). Deprecation warnings emitted if used.

**Stable** — permanent commitment. Breaking changes require a deprecation period of at least 2 release cycles (12 weeks), a migration path, and shim/adapter support during the transition.

Internal implementation (prompt engineering, context packing strategy, compaction algorithm, turn loop orchestration) carries no stability guarantees and can change freely between any two releases. This is the Linux kernel's `stable-api-nonsense.rst` principle applied to agent infrastructure.

The tool protocol (JSON-RPC over stdin/stdout) and tool manifest format (tool.toml) are treated as stable external contracts from v0.1 — they are the equivalent of the Linux syscall interface. Breaking changes to these formats would strand every external tool in the ecosystem.

---

## 11. Implementation Notes

### Language

Rust is the recommended implementation language. Rationale:
- Codex CLI's 80MB footprint vs OpenCode's 1GB+ demonstrates monolithic Rust's resource efficiency
- Cargo's feature system maps naturally to conditional compilation of provider/tool modules
- Cargo's crate system provides the module boundary we need
- Memory safety without GC — important for a runtime that manages scarce resources

### Crate structure (suggested)

```
agent-kernel/
├── crates/
│   ├── kernel-core/          # Turn loop, context manager, permission evaluator, session manager
│   ├── kernel-interfaces/    # ProviderInterface, ToolRegistration, etc. (stable)
│   ├── provider-anthropic/   # Anthropic provider module
│   ├── provider-openai/      # OpenAI provider module
│   ├── provider-ollama/      # Local model provider
│   ├── tool-filesystem/      # file_read, file_write, file_edit, grep, glob, ls
│   ├── tool-shell/           # shell executor
│   ├── tool-git/             # git operations
│   ├── tool-web/             # web_fetch
│   ├── tool-mcp-bridge/      # MCP client → ToolRegistration translator
│   ├── channel-tui-input/    # TUI as channel (human input events)
│   ├── frontend-tui/         # Reference TUI (control plane)
│   └── dist-code-agent/      # Reference coding agent distribution
├── policies/
│   ├── permissive.yaml
│   ├── lockdown.yaml
│   └── routing.yaml            # Channel event → session routing rules
├── skills/
│   ├── python-conventions.md
│   └── rust-conventions.md
├── MAINTAINERS
└── agent-kernel.toml
```

### What "done" looks like for v0.1

1. `cargo install agent-kernel` produces a working binary
2. `agent-kernel` in a project directory starts an interactive coding session
3. The agent can read files, edit files, run commands, and search — with permission prompts
4. Compaction fires at 65% and preserves task context
5. `request_tool` can load an MCP server's tools on demand
6. Switching from `policy/permissive.yaml` to `policy/lockdown.yaml` changes security posture without rebuilding
7. A different provider can be selected via config without touching any other code
8. The entire binary is under 100MB and uses under 100MB of RAM idle

---

## 12. What This Spec Does Not Cover (Deferred to v0.2+)

- **Tool registry / marketplace** — centralized registry with benchmarks, provenance, namespaced publishing
- **Benchmark harness** — tool-level and distribution-level benchmarking with leaderboards
- **L2 OS sandbox implementation** — seccomp-BPF, Seatbelt, namespace isolation
- **Multi-session support** — concurrent sessions, child spawning via `spawn_agent` tool
- **Channel modules** — GitHub webhook, Slack adapter, WhatsApp adapter, cron scheduler, file watcher
- **Channel event routing** — `notify_matching_session` delivery mode, session matching rules
- **Additional frontends** — Web UI, IDE extension, Slack/Teams bot
- **Context strategies as pluggable modules** — PageRank repo maps, embedding retrieval, tree-sitter AST summaries
- **Formal tool manifest format** (TOML-based, publishable to registry)
- **Cryptographic tool signing and provenance** (Sigstore-based)
- **Ecosystem flywheel infrastructure** — policy template sharing, distribution packaging
- **Python/TypeScript tool SDKs** — `@tool` decorator, auto-generated manifests, JSON-RPC boilerplate elimination

---

*Design principles: monolithic where coupling is necessary, modular where independence is possible. Security is structural, not optional. Governance enables velocity — it doesn't constrain it. Ship pragmatically — GNU Hurd pursued perfection for 35 years. Linux shipped.*
