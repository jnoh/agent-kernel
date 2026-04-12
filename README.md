<div align="center">

# agent-kernel

A Rust runtime for building AI agents.

[About](#about) | [Quickstart](#quickstart) | [Architecture](#architecture) | [Creating agents](#creating-agents) | [Development](#development)

</div>

## About

agent-kernel provides the core primitives for building AI coding agents: a turn loop, context management, tool dispatch, and permission evaluation. Agent "distributions" build on top of it the same way Linux distributions build on the Linux kernel — swap in different tools, policies, and prompts without touching the runtime.

The runtime is a single binary. Tools run as MCP subprocesses over JSON-RPC, so adding a new capability is shipping a binary and adding a line to a TOML manifest. The model never knows whether a tool is native Rust or a remote process.

**Key properties:**

- **Manifest-driven.** One TOML file defines the provider, tools, policy, and frontend. Different manifests produce different agents from the same binary.
- **MCP-native tooling.** Tools are subprocesses speaking JSON-RPC 2.0. First-party workspace tools (file read/write/edit, shell, ls, grep) ship as a built-in MCP server. Third-party MCP servers plug in with zero kernel changes.
- **Streaming.** Shell output streams line-by-line from subprocess to TUI while the command is still running. The model sees a single result at call end.
- **Policy-gated dispatch.** Every tool call passes through a permission evaluator before execution. YAML policy files declare allow/deny/ask rules per capability. Users can promote "ask" to "always allow" mid-session.
- **Session persistence.** Every context mutation is recorded to an append-only event log before touching in-memory state. Sessions can be replayed from disk.

## Quickstart

```sh
# Build
cargo build --workspace

# Put the MCP tool server on PATH
export PATH="$(pwd)/target/debug:$PATH"

# Run
cargo run -p agent-kernel -- --manifest distros/code-agent.toml
```

Set `ANTHROPIC_API_KEY` in your environment to use Claude. Without it, the binary falls back to an echo provider.

## Architecture

```
agent-kernel (single binary)
  |
  |-- loads manifest (distros/code-agent.toml)
  |-- builds provider (Anthropic / Echo)
  |-- spawns MCP tool subprocess (kernel-workspace-local)
  |-- creates session: turn loop + context + permissions
  |-- runs TUI or REPL frontend
  |
  |   Session thread              Main thread
  |   ┌─────────────────┐        ┌──────────────┐
  |   │ Turn loop        │───────>│ TUI / REPL   │
  |   │ Context manager  │  cross │ (renders     │
  |   │ Permission eval  │  beam  │  events,     │
  |   │ Tool dispatch    │<───────│  reads input)│
  |   └────────┬─────────┘        └──────────────┘
  |            │
  |     JSON-RPC 2.0 over stdio
  |            │
  |   ┌────────▼─────────┐
  |   │ MCP subprocess   │
  |   │ (workspace tools)│
  |   └──────────────────┘
```

### Crates

| Crate | Purpose |
|---|---|
| `kernel-interfaces` | Stable trait API: `ProviderInterface`, `ToolSet`, `ToolRegistration`, `FrontendEvents`, `PolicyInterface`. Shared types. |
| `kernel-core` | Runtime: turn loop, context manager, permission evaluator, session events, MCP stdio client, toolset pool. |
| `kernel-providers` | First-party provider implementations (Anthropic via `ureq`, Echo stub). |
| `kernel-workspace-local` | MCP server binary + library for the six workspace tools. |
| `agent-kernel` | The binary. Loads manifest, wires everything together, runs the frontend. |

## Creating agents

An agent is a manifest. Different manifests produce different agents from the same binary.

```toml
# distros/code-agent.toml — a coding assistant
[distribution]
name = "code-agent"
version = "0.1.0"

[provider]
type = "anthropic"
model = "claude-sonnet-4-5"
api_key_env = "ANTHROPIC_API_KEY"
fallback = "echo"

[policy]
file = "../policies/permissive.yaml"

[[toolset]]
kind = "mcp.stdio"
id = "workspace"
[toolset.config]
command = "kernel-workspace-local"
args = []
root = "."

[frontend]
type = "tui"
```

To create a different agent, write a different manifest:

- **Change the tools.** Point `command` at any MCP-compatible binary. Add multiple `[[toolset]]` entries to stack tool sets.
- **Change the policy.** Write a YAML policy that locks down or opens up capabilities.
- **Change the model.** Swap `claude-sonnet-4-5` for any Anthropic model, or use `type = "echo"` for testing.
- **Change the frontend.** `tui` for terminal, `repl` for simple line-based I/O.

```sh
agent-kernel --manifest distros/research-agent.toml
agent-kernel --manifest distros/devops-agent.toml
```

## Roadmap

| Status | Feature |
|--------|---------|
| Done | Manifest-driven configuration (provider, policy, tools, frontend) |
| Done | MCP stdio transport with streaming shell output |
| Done | Turn loop with tool dispatch and permission gating |
| Done | Context manager with tiered memory and compaction |
| Done | Session event persistence and replay |
| Done | TUI with markdown rendering, collapsible tool results |
| Planned | Web frontend (WebSocket) |
| Planned | Content-Length MCP framing for third-party server compatibility |
| Planned | Multi-session support |
| Planned | Snapshot-based context hydration |

## Development

```sh
# Full verify loop
cargo fmt -- --check && cargo clippy && cargo test

# Run tests for a specific crate
cargo test -p kernel-core

# Format
cargo fmt
```

The project uses a [spec-driven workflow](docs/spec-protocol.md). Each unit of work is scoped in a spec file under `specs/` before implementation begins.

## License

MIT
