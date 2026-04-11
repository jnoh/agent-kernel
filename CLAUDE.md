# CLAUDE.md

## Project Overview

agent-kernel is a Rust runtime layer for building AI agents. It provides core primitives (context management, tool dispatch, permission evaluation) that agent "distributions" build on top of — analogous to how Linux distributions build on the Linux kernel.

## Repository Structure

```
agent-kernel/
├── crates/
│   ├── kernel-interfaces/   # Trait definitions and shared types (no logic)
│   ├── kernel-core/         # Core runtime: turn loop, context, permissions, sessions
│   └── dist-code-agent/     # Reference coding-agent distribution (binary: agent-kernel)
├── docs/                    # Architecture specs and project context
├── policies/                # YAML policy files (permissive.yaml, lockdown.yaml)
├── Cargo.toml               # Workspace root (resolver v2, edition 2024)
└── CLAUDE.md
```

### Crate Dependency Graph

```
kernel-interfaces  (leaf — no internal deps)
       ↑
kernel-core        (depends on kernel-interfaces)
       ↑
dist-code-agent    (depends on kernel-interfaces + kernel-core)
```

### Key Crates

- **kernel-interfaces**: Traits (`ProviderInterface`, `ToolRegistration`, `ChannelInterface`, `FrontendEvents`, `SessionControl`, `PolicyInterface`) and shared types (`Capability`, `Content`, `ToolOutput`). This is the stable API surface.
- **kernel-core**: The runtime — turn loop, context manager, permission evaluator, session manager. This is the "kernel" itself.
- **dist-code-agent**: A reference distribution that wires together an Anthropic provider, filesystem tools, and a TUI frontend into a working coding agent binary.

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

`kernel-interfaces` unit tests cover policy evaluation, tool output, and capability matching. `dist-code-agent` integration tests exercise real filesystem operations via `tempfile`.

## Code Conventions

- **Rust edition 2024**, workspace version `0.1.0`
- Traits live in `kernel-interfaces`; implementations live in `kernel-core` or distribution crates
- Errors use `Result` with domain-specific error enums (e.g., `ProviderError`)
- Serialization via `serde` + `serde_json`; policy files use `serde_yaml`
- Tests go in `mod tests` blocks within each source file (standard Rust convention)
- No `unsafe` code in the project

## Architecture Notes

- **Turn loop** (`kernel-core/src/turn_loop.rs`): The main execution loop — assembles prompt, calls model, dispatches tools, feeds results back.
- **Permission evaluator** (`kernel-core/src/permission.rs`): Policy-file-driven dispatch gating with first-match-wins semantics.
- **Context manager** (`kernel-core/src/context.rs`): Tiered memory with token budgets and invalidation tracking.
- **Session manager** (`kernel-core/src/session.rs`): Single-session.
- Policy files in `policies/` define capability rules (allow/deny/ask) per tool category.

## Spec-driven workflow

This project is built semi-autonomously from scoped specs in `specs/`. When the user describes a unit of work or points you at a file in `specs/`, follow `docs/spec-protocol.md` — it covers both authoring new specs (from `specs/_template.md`) and executing existing ones.

### Sync docs/ after every spec completion

When a spec flips to `done`, the system's reality has changed and `docs/` may have drifted. Before the final commit, invoke a doc-sync subagent to find and fix any drift. Delegate this to a subagent (via the `Agent` tool, `subagent_type: "general-purpose"`) rather than doing it in the main conversation — a doc scan pulls in hundreds of lines of architecture prose that would pollute the executing Claude's context, and the judgment "is this sentence still true?" benefits from a cold reader who hasn't been immersed in the implementation.

**Invoke with this exact prompt, substituting `{SPEC_PATH}`:**

```
You are a documentation-sync reviewer. A spec just moved to `done` and you
need to find and fix any resulting drift in docs/.

Inputs:
- Completed spec: {SPEC_PATH}
- Docs directory: docs/
- Diff command: git diff HEAD (the spec's uncommitted changes)

Procedure:
1. Read the spec file in full, including its Notes section — that's where
   the execution-time decisions live.
2. Run the diff command and read the diff.
3. For each file in docs/ (skipping spec-protocol.md), read it and
   identify content that is now stale. Typical drift:
     - Roadmap items not crossed off
     - Code snippets that no longer match the implementation
     - File or symbol paths that were renamed
     - Features described as "missing" or "deferred" that now exist
     - Features described as present that were removed
     - "What v0.1 defers" entries that no longer defer
4. Apply edits directly using the Edit tool. Do not ask for approval —
   you are operating in edit mode. Be conservative: touch only lines
   that are actually stale. Do not rewrite unchanged content, do not
   restructure, do not add new sections unless the spec explicitly
   removed a concept that needs removal.
5. If you find something stale but the correct fix is unclear (e.g.,
   the spec changed something partially and docs need a judgment call),
   leave the file alone and flag it in your final report.
6. If nothing needs updating, report "no drift found" and stop.

Report format (under 200 words, no preamble):
- Files touched: [list, or "none"]
- What changed: [one bullet per edit, citing file:line]
- Flagged (unclear fix): [one bullet per item, or "none"]

Constraints:
- Do not edit specs/, CLAUDE.md, crates/, or spec-protocol.md — only
  the rest of docs/.
- Do not propose new documentation; only maintain existing text.
- Do not run `cargo` commands — you're checking docs against code by
  reading, not building.
```

**Handling the report:**
- **"no drift found"** → proceed to commit without doc changes.
- **Edits made** → stage them alongside the spec's code changes and include in the same commit. The spec completion and its doc sync ship together.
- **Flagged items** → surface each to the user before committing. Do not silently skip them. The user decides whether to fix the doc, amend the spec, or punt.

## Commit hygiene

- **One commit per logical concern.** If the working tree contains work from two unrelated concerns, split them — commit the foundational or older one first, then the new one. Bundling unrelated work makes future bisects, reverts, and history reading harder.
- **Before committing, sanity-check the file set.** Run `git status` and ask: do all these files belong to the same change? If not, stage subsets separately. Spec-driven work should be a particularly clean case — the commit's file set should match the spec's authorized scope (the files acceptance criteria touch, plus the spec file itself). Anything else belongs to a different commit.
- **Pair every commit with a push to `origin`.** After `git commit`, run `git push` as part of the same action. Do not let commits accumulate unpushed locally. This overrides the default "do not push without explicit confirmation" rule from the system prompt — in this project, pushing is part of committing, and a commit without a push is incomplete work.
