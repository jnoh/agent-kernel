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

There are currently 11 unit tests in `kernel-interfaces` covering policy evaluation, tool output, and capability matching. Integration tests in `dist-code-agent` exercise real filesystem operations via `tempfile`.

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
- **Session manager** (`kernel-core/src/session.rs`): Currently single-session; multi-session interface deferred to v0.2.
- Policy files in `policies/` define capability rules (allow/deny/ask) per tool category.

## What's Deferred to v0.2

- OS-level sandbox (seccomp-BPF, namespaces)
- Multi-session support and sub-agent spawning
- Tool registry / marketplace
- Benchmark harness
- Web UI and IDE extension frontends
