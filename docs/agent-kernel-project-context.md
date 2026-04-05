# agent-kernel: Project Context for New Conversations

**Upload this document to any new Claude conversation to transfer full architectural context.**

*Last updated: April 5, 2026. Generated from ~18 hours of architectural design work.*

---

## What This Project Is

agent-kernel is an open-source runtime layer for building AI agent harnesses. It sits below application-level agent frameworks (LangGraph, CrewAI) and above model APIs and protocols (MCP, A2A). It is NOT an agent — it is the thing agents are built on. Different agents (coding, support, legal, CI) are "distributions" that package the kernel with domain-specific tools, policies, and frontends. The relationship is Linux kernel to Ubuntu/Fedora/Android.

The kernel provides four core primitives no existing framework unifies: tiered context management, capability-based tool dispatch, defense-in-depth security, and session lifecycle management.

---

## Core Architecture (Settled Decisions)

### Four core components (monolithic, cannot be swapped):

1. **Turn Loop** — receive input → construct prompt → call model → parse tool calls → dispatch through permission evaluator → feed results back. Single-threaded per session. Does not know what model, tools, or frontend it's talking to.

2. **Context Manager** — owns the token budget with three tiers:
   - Tier 1 Working Memory: reserved, never evicted (system prompt, scratchpad, current turn)
   - Tier 2 Short-Term: progressively summarized (conversation history, tool results)
   - Tier 3 Long-Term: outside context entirely, accessible via tool calls
   - Key insight: "Context is not memory — context is attention." Reframes compaction from "what to evict" to "what should the model be thinking about right now."
   - Implements `request_tool` meta-tool for demand-paging tool definitions
   - Compaction triggers at 60-70% (not 80-95% like competitors), with death spiral guards

3. **Permission Evaluator** — dispatch gate intercepting every tool invocation. Checks declared capabilities against loaded policy. Returns allow/deny/ask. Provides mechanism only — policy is external config. Must be in core because if it were a module, it could be unloaded.

4. **Session Manager** — process table (singleton). Owns all running sessions. Single entry point for session creation. Three spawn triggers: human starts conversation, running session spawns child, external event arrives via channel. Manages global resource budget, routes invalidations between sessions. v0.1 manages exactly one session; interface designed for multi-session in v0.2.

### Five module interfaces (stable contracts, backward-compat guarantees):

1. **ProviderInterface** — complete/stream/count_tokens/capabilities. The turn loop calls these; never touches provider-specific APIs.

2. **ToolRegistration** — the chokepoint contract. Every tool declares:
   - `capabilities`: Set<Capability> — what resources it touches (fs:read, net:*, shell:exec)
   - `schema`: ToolSchema — JSON Schema for model to call
   - `cost`: TokenEstimate — context budget this tool's schema consumes
   - `relevance`: RelevanceSignal — when to demand-page this tool in
   - `execute(input) -> Result<ToolOutput>` — the work
   - Three implementation paths: native Rust (in-process), external process (JSON-RPC over stdin/stdout), MCP bridge. All produce the same `Box<dyn ToolRegistration>`.

3. **ChannelInterface** — how external events enter the kernel (data plane). Channels are pluggable modules that produce events: webhooks, Slack, WhatsApp, cron, file watchers, TUI input. They translate external protocols into ExternalEvent and hand to session manager.

4. **FrontendInterface** — how humans manage the system (control plane). Rich, bidirectional. Can list sessions, switch between them, inspect context, view audit logs, manage policies, promote autonomous→interactive. Distinct from channels: channels create sessions and deliver events; frontends observe, manage, and control sessions.

5. **PolicyInterface** — external policy configuration (YAML files). Mechanism/policy separation: same binary, different policy file = developer laptop vs enterprise CI.

### Invalidation Protocol (part of ToolRegistration):
Every `ToolOutput` includes optional `invalidations` vector:
- `Files(paths)` — cached file contents stale
- `WorkingDirectory(path)` — workspace root changed (subject to policy check)
- `ToolRegistry` — available tools changed
- `Environment(vars)` — env vars changed
Turn loop processes invalidations between turns, never mid-turn. Session manager routes invalidations across sessions with overlapping cached state.

### Security: three layers (defense in depth):
- L1 Dispatch Gate: capabilities vs policy, before execution
- L2 OS Sandbox: seccomp-BPF/Seatbelt, during execution (v0.1: stub)
- L3 Resource Budgets: tokens, wall time, memory, output size (cgroups equivalent)

### Session Model:
- Sessions are isolated — own turn loop, context manager, permission evaluator, tools, frontend, budget
- Interactive sessions: human attached, can answer permission prompts
- Autonomous sessions: all decisions from policy, Permission::Ask → Permission::Deny
- Autonomous can be promoted to interactive when hitting policy limits
- Events queue in `pending_results`, drained between turns (never mid-turn)
- No inter-session messaging. Communication via filesystem + invalidation protocol, or parent-as-coordinator

### Tool Ecosystem:
- The kernel ships only 2 built-in tools: `request_tool` (demand-paging) and `plan` (scratchpad access)
- All other tools are distribution concerns — distributions bundle tools as external modules
- SDK eliminates JSON-RPC boilerplate: `@tool` decorator in Python/TS/Go generates everything from type hints
- Three paths: native Rust crate (linked at build time via Cargo features), external process (JSON-RPC), MCP bridge
- Tool.toml manifest declares capabilities, schema, cost, relevance — the only thing the registry indexes

### Distribution Model:
- A distribution is a manifest of manifests: tools + policy + skills + provider config + frontend
- Same distribution installs differently based on environment policy overrides
- Distribution authors never touch Rust — they write Python tools, YAML policies, markdown skills
- Distributions can package custom plugins that ship inside the distribution

---

## Key Architectural Insights

- **Context is not memory — context is attention.** Unlike OS memory (passive storage), LLM context actively shapes every token generated.
- **The microkernel tax inverts for agent tools.** IPC overhead (~5ms) is negligible vs LLM inference (1-5 seconds). Tool isolation is essentially free.
- **Mechanism vs policy separation** at every level. Same binary, different config = different security posture.
- **Skills inform the model, tools inform the kernel.** Skills are prompt-level (no capabilities, no security participation). Tools are runtime-level (machine-readable metadata, benchmarkable, participate in security/context/dispatch).
- **`request_tool` is unique** — demand-pages tool definitions based on what the model needs. Claude Code's ToolSearch does something similar but only for MCP tools; ours covers the entire registry.
- **The invalidation protocol was discovered, not designed** — the git worktree tool exercise revealed that tools modifying the environment need to signal changes back to the kernel.
- **Extensions are retention, not acquisition** (VS Code lesson). First 1,000 users come from core experience quality, not ecosystem.

---

## Competitive Landscape (Researched)

- **OpenCode** (116K+ stars): No context management (loads all tools every call), no mechanism/policy separation, no invalidation protocol, no session model, application-level security only. Good DX with Tool.define() + Zod.
- **Claude Code**: Closest to our design in tool granularity and subagent model. Has ToolSearch (similar to request_tool), isolated WebFetch context (security pattern we adopted), ML-based auto mode for permissions. Monolithic TypeScript, no stable interfaces.
- **OpenClaw** (348K stars): Gateway→Channel→Agent architecture. "Channels" terminology adopted for our design. Multi-agent with sub-agent spawning. Lobster workflow engine. Single Node.js process.
- **OpenFang** (15.8K stars, Rust): Closest to full kernel with 16-layer security. No context management.
- **Codex CLI** (Rust, 80MB): Minimal tool set (shell + apply_patch + web_search). OS-level sandboxing (Seatbelt/Landlock). Performance benchmark for what Rust achieves.
- **No project implements** the combination of: tiered context management + capability-based dispatch + defense-in-depth security + stable module interfaces + demand-paging.

---

## Implementation Decisions

- **Language: Rust** — 80MB vs 1GB+ (Codex vs OpenCode), Cargo's module system, memory safety without GC. Contributor pool is smaller but filters for systems thinkers. Go is the alternative for faster contributor growth.
- **Crate structure**: kernel-core, kernel-interfaces (stable), provider-*, tool-*, channel-*, frontend-*, dist-*
- **v0.1 "done" criteria**: cargo install produces working binary, interactive coding session works, compaction fires at 65%, request_tool loads MCP tools, policy file swap changes security posture, binary <100MB, RAM <100MB idle.

---

## Open Questions / Pending Decisions

1. **Are channels and frontends truly separate?** Current decision: yes — channels are data plane (event pipes), frontends are control plane (session management). TUI implements both interfaces. This is settled but could be revisited if it proves awkward in practice.

2. **How does the plan/scratchpad tool interact with persistent Tasks?** Claude Code has TodoWrite (in-memory, per-session) AND Tasks (file-based, persistent, dependencies). Our `plan` tool maps to TodoWrite. A persistent `task` tool for cross-session work is a v0.2 candidate.

3. **WebFetch isolated context pattern** — should tools that ingest untrusted external content run through an isolated context window? Adopted from Claude Code's design. Needs implementation detail in the spec.

4. **ML-augmented permission evaluation** — Claude Code's auto mode uses ML classifiers for permission decisions (84% fewer prompts). Our PolicyInterface.evaluate() can support this without interface changes. Worth designing for in v0.2.

5. **Non-coding distributions** — what specific non-coding agent distributions would demonstrate the kernel's generality? Support agent? Legal agent? Research agent?

6. **GNU Hurd risk** — the spec is comprehensive at 1,030 lines. Does it need to be smaller for v0.1? The answer is probably: build the kernel-interfaces crate first (the sacred boundary), then kernel-core, and defer everything else.

---

## Documents Produced

1. **AGENT_KERNEL.md** (or agent-kernel-v01-spec.md) — Complete v0.1 buildable specification (12 sections, ~1,138 lines). The authoritative document. Includes three-path tool implementation, SDK decorator pattern, distribution packaging model, channel/frontend separation, and API maturity staging.
2. **agent-kernel-project-context.md** — This file. Transfer document for new conversations.
3. **agent-architecture.jsx** — Interactive React architecture diagram with 6 tabbed sections.
4. **tools/git-worktree/** — Reference tool implementation (tool.toml + worktree.py) demonstrating the external tool protocol and invalidation system.
5. **Research artifacts** (in conversation history, not separate files):
   - Linux kernel lessons for agent builders
   - Kernel architecture comparison (monolithic/micro/hybrid/exokernel)
   - OpenCode tool system analysis
   - Agent kernel competitive landscape (20+ projects)
   - Module ecosystem design principles
   - OpenClaw architecture and multi-agent patterns
   - Claude Code complete tool reference (~40 tools documented)
   - Go-to-market strategy for open-source infrastructure
   - Version evolution strategies (Linux, K8s, Rust, Docker, VS Code, Terraform)

---

## How to Use This Document

### In Claude Code
Place `AGENT_KERNEL.md` in your project root. Claude Code will read it as project context automatically. Use this context document as a reference when Claude Code needs background on architectural decisions.

```bash
# In your project directory:
cp AGENT_KERNEL.md .
# Claude Code will now have the full spec as context

# To start building:
claude "Read AGENT_KERNEL.md and scaffold the kernel-core and kernel-interfaces crates"
```

### In Claude.ai conversations
**To continue the design:** Upload this file + AGENT_KERNEL.md. Ask about any open question or push on any settled decision.

**To start building:** Upload AGENT_KERNEL.md. It's the buildable spec. Start with kernel-core and kernel-interfaces crates.

**To explore a specific subsystem:** Upload this file and ask about context management, security, sessions, channels, tools, or distributions specifically.

**To challenge the architecture:** Upload this file and propose alternatives. Every decision has documented rationale that can be interrogated.
