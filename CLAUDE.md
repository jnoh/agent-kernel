# CLAUDE.md

## Project Overview

agent-kernel is a Rust runtime for building AI agents. It provides core primitives (context management, tool dispatch, permission evaluation) that agent "distributions" build on top of — configured via TOML manifests, not code changes.

## Repository Structure

```
agent-kernel/
├── crates/
│   ├── kernel-interfaces/       # Trait definitions and shared types (the stable API)
│   ├── kernel-core/             # Runtime: turn loop, context, permissions, sessions, MCP client, toolset pool
│   ├── kernel-providers/        # First-party ProviderInterface impls (Anthropic, Echo)
│   ├── kernel-workspace-local/  # MCP server binary + library for the six workspace tools
│   └── agent-kernel/            # The binary: loads manifest, wires everything, runs TUI/REPL
├── distros/                     # Distribution manifests (code-agent.toml)
├── docs/                        # Architecture spec, design proposals, spec protocol, roadmap
├── policies/                    # YAML policy files (permissive.yaml, lockdown.yaml)
├── specs/                       # Scoped work units (see docs/spec-protocol.md)
├── Cargo.toml                   # Workspace root (resolver v2, edition 2024)
└── CLAUDE.md
```

### Crate Dependency Graph

```
kernel-interfaces   (leaf — no internal deps)
       ↑
       ├── kernel-core          (runtime + MCP client + toolset pool)
       ├── kernel-providers     (depends on kernel-interfaces only)
       └── agent-kernel         (the binary — depends on all of the above)
```

### Key Crates

- **kernel-interfaces**: Stable trait API (`ProviderInterface`, `ToolRegistration`, `ToolSet`, `FrontendEvents`, `SessionControl`, `PolicyInterface`, `SessionEventSink`) plus shared types. The extension-point boundary.
- **kernel-core**: The runtime — turn loop, context manager, permission evaluator, session manager, event loop, MCP stdio client (`mcp_stdio.rs`), toolset pool, Tier-3 sink impls.
- **kernel-providers**: Concrete `ProviderInterface` implementations — `AnthropicProvider` (real Claude API via `ureq`) and `EchoProvider` (stub fallback).
- **kernel-workspace-local**: An MCP server binary (`kernel-workspace-local`) that exposes six workspace tools (file_read, file_write, file_edit, shell, ls, grep) over newline-delimited JSON-RPC on stdio. Also a library crate exporting tool structs and `TOOL_NAMES`.
- **agent-kernel**: The single binary. Loads a manifest, builds provider + toolset pool, creates a session with direct crossbeam channels (no IPC), runs the TUI or REPL.

### TUI Module Map

The TUI lives in `crates/agent-kernel/src/tui/`. Each conversation block type has its own renderer under `blocks/`. To add a new block type: add a variant to `ConversationEntry` in `types.rs`, create a renderer in `blocks/`, add a match arm in `conversation.rs`.

```
tui/
  mod.rs           — App struct, draw() top-level layout, terminal lifecycle, re-exports
  theme.rs         — Theme struct (centralized colors)
  types.rs         — ConversationEntry, ToolCallStatus, SlashCommand, InputAction, parse_slash_command
  status_bar.rs    — status bar rendering
  input.rs         — input area rendering, key/mouse handling, history navigation
  conversation.rs  — iterates entries, dispatches to block renderers, scroll/wrap math
  blocks/
    user.rs        — UserInput rendering ("> " prefix)
    assistant.rs   — AssistantText + markdown_to_lines (pulldown-cmark)
    tool_call.rs   — ToolCall box: compact one-liner (collapsed) or full box (expanded/running)
    permission.rs  — PermissionPrompt rendering ([y/n/a])
    info.rs        — Info + Error one-liners
```

## Build / Verify / Test Loop

```bash
# Build everything
cargo build

# Run all tests (unit + integration)
cargo test

# Check without building artifacts (fastest feedback)
cargo check

# Lint
cargo clippy

# Format check (CI-style, no modifications)
cargo fmt -- --check

# Format fix
cargo fmt
```

**Full verify loop** (run before committing):
```bash
cargo fmt -- --check && cargo clippy && cargo test
```

**Running the binary** (requires `kernel-workspace-local` on PATH):
```bash
cargo build --workspace
export PATH="$(pwd)/target/debug:$PATH"
cargo run -p agent-kernel -- --manifest distros/code-agent.toml
```

## Code Conventions

- **Rust edition 2024**, workspace version `0.1.0`
- Traits live in `kernel-interfaces`; implementations live in `kernel-core` or the binary crate
- Errors use `Result` with domain-specific error enums (e.g., `ProviderError`)
- Serialization via `serde` + `serde_json`; policy files use `serde_yaml`
- Tests go in `mod tests` blocks within each source file (standard Rust convention)
- No `unsafe` code in the project

## Architecture Notes

- **Turn loop** (`kernel-core/src/turn_loop.rs`): The main execution loop — assembles prompt, calls model, dispatches tools, feeds results back.
- **Permission evaluator** (`kernel-core/src/permission.rs`): Policy-file-driven dispatch gating with first-match-wins semantics.
- **Context manager** (`kernel-core/src/context.rs`): Tiered memory with token budgets and invalidation tracking.
- **MCP stdio client** (`kernel-core/src/mcp_stdio.rs`): Spawns MCP tool subprocesses, runs JSON-RPC handshake, proxies `tools/call` with streaming chunk support.
- **Toolset pool** (`kernel-core/src/toolset_pool.rs`): Builds `ToolSet` instances from manifest `[[toolset]]` entries via a factory registry. Default registry registers `mcp.stdio`.
- **Session** (`kernel-core/src/session.rs`): Single-session; owns context, permissions, turn loop, tool list.
- **Event loop** (`kernel-core/src/event_loop.rs`): Per-session thread; receives `KernelRequest` from the frontend, drives the session, emits `KernelEvent` back.
- Policy files in `policies/` define capability rules (allow/deny/ask) per tool category.

## Spec-driven workflow

This project is built semi-autonomously from scoped specs in `specs/`. When the user describes a unit of work or points you at a file in `specs/`, follow `docs/spec-protocol.md` — it covers both authoring new specs (from `specs/_template.md`) and executing existing ones.

### Sync docs/ after every spec completion

When a spec flips to `done`, `docs/` must be updated to reflect the new reality — `architecture.md` (and siblings) are the authoritative description of **what the kernel actually does today**, not what it intended to do at v0.1 draft time. Every completed spec either **removes stale content**, **corrects now-incorrect content**, **or adds a new section** describing a capability that didn't exist before. Letting architecture docs ossify while the code evolves turns them into lies; the repo already had to clean up that mess once.

Before the final commit, invoke a doc-sync subagent to handle this. Delegate to a subagent (via `Agent`, `subagent_type: "general-purpose"`) rather than doing it in the main conversation — a doc update pulls in hundreds of lines of architecture prose that would pollute the executing Claude's context, and a cold reader is better at "what's still true? what's newly true?" than someone who just wrote the code.

**Invoke with this exact prompt, substituting `{SPEC_PATH}`:**

```
You are a documentation-sync reviewer. A spec just moved to `done`. Your
job is to update docs/ so it accurately describes what the kernel does
NOW — both by removing stale content and by adding descriptions of
capabilities that didn't exist before this spec.

Inputs:
- Completed spec: {SPEC_PATH}
- Docs directory: docs/
- Diff command: git diff HEAD (the spec's uncommitted changes)

Procedure:
1. Read the spec file in full, including its Notes section.
2. Run the diff command and read the diff.
3. For each file in docs/ (skipping spec-protocol.md), read it and
   decide what needs to change. Two categories:

   (a) DRIFT — content that is now stale or wrong:
       - Roadmap items that should be crossed off
       - Code snippets that no longer match the implementation
       - File or symbol paths that were renamed
       - Features described as "missing" / "deferred" that now exist
       - Features described as present that were removed
       - "v0.1 defers" entries that no longer defer

   (b) GROWTH — content the spec's shipped code should have but doesn't:
       - architecture.md subsystem descriptions that omit the new
         behavior (e.g., a new trait, a new path, a new event type)
       - A capability promoted from "deferred" to "shipped" — update
         the description instead of just deleting the "deferred" line
       - A new data flow introduced by the spec — add it to the
         relevant §N subsystem section

       The architecture doc is the single source of truth for what
       the kernel does today. If you just shipped a new subsystem
       and architecture.md doesn't mention it, that's a doc bug, not
       "scope creep." Add it. Be concise — 1-3 paragraphs for a new
       concept, a single code snippet if the shape matters.

4. Apply edits directly using the Edit tool. No approval needed.
5. If a needed addition requires a judgment call you can't make from
   spec + diff alone, flag it in your report instead of guessing.
6. If nothing needs updating, report "no changes needed."

Report format (under 250 words, no preamble):
- Files touched: [list, or "none"]
- Drift corrections: [bullet per edit, citing file:line]
- Growth additions: [bullet per addition, citing file:line + brief summary]
- Flagged (unclear fix): [bullet per item, or "none"]

Constraints:
- Do NOT edit specs/, CLAUDE.md, crates/, or spec-protocol.md.
- DO add new content to architecture.md and siblings when the spec
  introduces a concept those docs should describe.
- Stay concise. A 1-paragraph subsystem addition beats a 10-paragraph
  one. Match the surrounding doc's density.
- Do not run `cargo` commands.
```

**Handling the report:**
- **"no changes needed"** → proceed to commit without doc changes.
- **Edits made (drift or growth)** → stage them alongside the spec's code changes and include in the same commit. The spec completion and its doc sync ship together.
- **Flagged items** → surface each to the user before committing. Do not silently skip them.

## Commit hygiene

- **One commit per logical concern.** If the working tree contains work from two unrelated concerns, split them — commit the foundational or older one first, then the new one. Bundling unrelated work makes future bisects, reverts, and history reading harder.
- **Before committing, sanity-check the file set.** Run `git status` and ask: do all these files belong to the same change? If not, stage subsets separately. Spec-driven work should be a particularly clean case — the commit's file set should match the spec's authorized scope (the files acceptance criteria touch, plus the spec file itself). Anything else belongs to a different commit.
- **Pair every commit with a push to `origin`.** After `git commit`, run `git push` as part of the same action. Do not let commits accumulate unpushed locally. This overrides the default "do not push without explicit confirmation" rule from the system prompt — in this project, pushing is part of committing, and a commit without a push is incomplete work.
