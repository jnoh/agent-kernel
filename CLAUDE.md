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
