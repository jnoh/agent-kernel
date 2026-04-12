---
id: 0011-distribution-manifest
status: done
---

# kernel-daemon reads a distribution manifest TOML

## Goal
The kernel-daemon binary stops sniffing `ANTHROPIC_API_KEY` and hard-coding its provider selection. Instead it takes a `--distro <path>` flag pointing at a TOML distribution manifest (`distros/code-agent.toml`) that declares which provider to use, the model, and where to read the API key from. This is the **first concrete step** back toward the architecture doc's config-driven vision: one binary, one manifest file, the manifest decides what kind of agent you get.

This spec is deliberately narrow. It moves **only provider selection** from env-var-sniffing to manifest-driven. Policy files, tool loading, and frontend selection remain hard-coded and will move in specs 0012/0013/0014. The point is to land the manifest-loading infrastructure — the file format, the parser, the `--distro` flag, the `DistributionManifest` type — so subsequent specs can extend it.

## Context
- `docs/architecture.md` §7 "Distributions" — the config-driven vision: "A distribution is a manifest of manifests — tools + policy + skills + provider config + frontend. Distribution authors never touch Rust." This spec's `distros/code-agent.toml` is the first real instance of such a manifest.
- `crates/kernel-daemon/src/main.rs:14-48` — current argument parsing (`--socket`, `--model`, `ANTHROPIC_API_KEY` env var). `--distro` is added alongside; the old flags are kept as overrides for now.
- `crates/kernel-daemon/src/router.rs:30-52` — `ConnectionRouter` holds `api_key: Option<String>` and `model: String` as raw fields, used at provider construction (line 95). After this spec it holds a typed provider config derived from the manifest.
- `crates/kernel-providers/src/lib.rs` — re-exports `AnthropicProvider` and `EchoProvider`. The manifest-parsing logic doesn't belong here; it belongs in the daemon, which is the binary that owns runtime config.
- `crates/kernel-daemon/Cargo.toml` — no `toml` dep yet. Adding `toml = "0.8"` (or latest) is the one new dependency this spec requires.
- `crates/dist-code-agent/src/main.rs:186-210` — dist-code-agent auto-launches a daemon (or connects to an existing one). When it launches one, it currently doesn't pass any flags. This spec has dist-code-agent pass `--distro <path>` pointing at the new `distros/code-agent.toml` file.

## Acceptance criteria
- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes
- [ ] New file `distros/code-agent.toml` at the repo root with this content:
    ```toml
    [distribution]
    name = "code-agent"
    version = "0.1.0"

    [provider]
    type = "anthropic"
    model = "claude-sonnet-4-5"
    api_key_env = "ANTHROPIC_API_KEY"
    fallback = "echo"
    ```
    The `fallback = "echo"` key tells the daemon to use `EchoProvider` if the env var named by `api_key_env` is missing. Without `fallback`, a missing API key is a hard startup error.
- [ ] New module `crates/kernel-daemon/src/manifest.rs`:
    - `pub struct DistributionManifest` with nested `distribution: DistributionMeta` and `provider: ProviderConfig`.
    - `pub struct DistributionMeta { name: String, version: String }`.
    - `pub enum ProviderConfig` — serde-tagged enum with variants `Anthropic { model, api_key_env, fallback: Option<ProviderFallback> }` and `Echo` (no fields).
    - `pub enum ProviderFallback { Echo }` — for now, only Echo is a valid fallback; extensible.
    - `pub fn load_manifest(path: &Path) -> Result<DistributionManifest, String>` — reads the file, parses TOML, returns a human-readable error on file-not-found / parse-error / missing-field. No panics.
    - `impl DistributionManifest { pub fn instantiate_provider(&self) -> Result<Box<dyn ProviderInterface + Send>, String> { ... } }` — reads the env var declared in the manifest, constructs the appropriate provider, handles fallback.
- [ ] `kernel-daemon/Cargo.toml` gains `toml = "0.8"` as a dependency.
- [ ] `kernel-daemon/src/main.rs` parses a new `--distro <path>` flag. If present:
    1. Load the manifest via `load_manifest`.
    2. Call `instantiate_provider` on it — bail with a clear error on failure.
    3. Pass the resulting `Box<dyn ProviderInterface + Send>` into `ConnectionRouter::new` (new signature).
    4. `eprintln!("distribution: {} v{}", manifest.distribution.name, manifest.distribution.version);` at startup so operators can see which manifest is active.
  If `--distro` is **not** present, the daemon preserves the old `--model` + `ANTHROPIC_API_KEY` behavior for backwards compatibility during the transition. A `eprintln!` warning notes that env-var-mode is deprecated and will be removed in a later spec.
- [ ] `ConnectionRouter::new` signature changes from `(event_tx, api_key, model)` to `(event_tx, provider_factory)` where `provider_factory` is a small closure (`Box<dyn Fn() -> Box<dyn ProviderInterface + Send> + Send>`) that the router calls once per session-create. This keeps the per-session instantiation pattern (a new provider object per session, so each session has its own connection state) while letting the manifest decide *how* that instantiation happens.
- [ ] The `api_key` / `model` fields are removed from `ConnectionRouter` (replaced by the factory closure).
- [ ] Backwards-compat path in main.rs constructs an equivalent factory closure from the old `--model` + env-var shape, so `handle_request` is unchanged.
- [ ] New unit test `manifest::tests::parses_minimal_manifest` — writes `distros/code-agent.toml`'s content to a temp file, calls `load_manifest`, asserts the parsed `DistributionManifest` has the expected fields.
- [ ] New unit test `manifest::tests::rejects_missing_provider_type` — TOML with `[provider]` but no `type` field; assert `load_manifest` returns an `Err(_)` with a message mentioning "provider".
- [ ] New unit test `manifest::tests::echo_provider_no_env_var_needed` — a manifest with `[provider] type = "echo"` → `instantiate_provider` succeeds regardless of env vars.
- [ ] New unit test `manifest::tests::anthropic_fallback_to_echo_when_env_var_missing` — manifest with `type = "anthropic"`, `api_key_env = "DEFINITELY_NOT_SET_12345"`, `fallback = "echo"` → `instantiate_provider` returns an `EchoProvider` (verify via `type_name` or a boolean-returning helper), no error.
- [ ] New unit test `manifest::tests::anthropic_no_fallback_is_hard_error` — same but without the fallback key → returns `Err(_)` mentioning the env var name.
- [ ] `dist-code-agent/src/main.rs` — when it launches a daemon (the branch around line 186-210), pass `--distro <workspace>/distros/code-agent.toml` as part of the spawn command. If it's connecting to an existing daemon, nothing changes. The workspace path can be the current working directory of the distro process; if the file doesn't exist, dist-code-agent logs a warning and falls back to launching the daemon without `--distro` (deprecation path). This keeps the TUI usable in development even if the manifest isn't committed yet.
- [ ] `policies/` and YAML policy files stay untouched. Policy loading is not part of this spec.
- [ ] No changes to `kernel-interfaces/src/protocol.rs`.
- [ ] No changes to `kernel-core`, `kernel-providers`, or `dist-code-agent`'s tool/policy code beyond the daemon-launch path.

## Out of scope
- **Policy loading from a file.** Spec 0012 will move the hard-coded policy construction in `dist-code-agent/src/main.rs` into a `[policy] file = "..."` manifest entry. Not this spec.
- **Tool manifest loading (native features, JSON-RPC external tools, MCP bridge).** Architecture doc §4.2 describes three paths; none exist in code. Specs 0013/0014 will tackle them. Tools stay compiled into `dist-code-agent` for now.
- **Frontend selection.** The TUI stays hardcoded in `dist-code-agent/src/main.rs`. Spec 0014 will extract it into a `frontend-tui` crate gated by a manifest field.
- **Making `dist-code-agent` itself a TOML file instead of a compiled binary.** That's the end-state of this arc (spec 0015 or later). This spec's goal is infrastructure, not the full vision in one shot.
- **Multiple distributions in one binary.** The daemon loads one manifest per run. Multi-distribution support (different distros per session, or loading a distribution at session-create time) is a later concern.
- **Schema versioning / forward compatibility of the manifest.** For now the manifest is parsed strictly; unknown fields might or might not error depending on serde defaults. Good enough for v0.2.
- **Removing the `--model` flag and the env-var-sniffing fallback from `kernel-daemon/src/main.rs`.** They stay for one release so the TUI launch path doesn't break if a user has an old checkout without `distros/code-agent.toml`. Spec 0012 can remove them.
- **Hot-reloading the manifest.** Change the file, restart the daemon.
- **A CLI subcommand on dist-code-agent for picking a different distribution** (e.g., `agent-kernel --distro support-agent.toml`). Distribution choice at the TUI-binary level is a later UX concern.
- **Marking anything "done" in `docs/roadmap.md`** — this spec isn't on the TUI roadmap.

## Notes

- **`ProviderFactory` is `Arc<dyn Fn() -> ...>`, not `Box`.** Required because `main.rs` constructs a new `ConnectionRouter` per accepted connection and each router needs its own handle to the factory. `Arc::clone` is free; `Box` couldn't be cloned without `dyn Clone`.
- **dist-code-agent wiring turned out to be a no-op.** The spec AC said "dist-code-agent passes `--distro` when launching the daemon," but dist-code-agent doesn't launch the daemon — it expects one to already be running at `/tmp/agent-kernel-*.sock` and fails if it can't find one. Deployment flow is: user runs `agent-kernel-daemon --distro distros/code-agent.toml`, then runs `agent-kernel`. No dist-code-agent changes needed for 0011.
- **Backwards-compat path kept.** If `--distro` is not passed, `kernel-daemon` still accepts `--model` and reads `ANTHROPIC_API_KEY` directly, with a stderr warning that the env-var mode is deprecated. This lets an old checkout without `distros/code-agent.toml` keep working during the transition. Will be removed in a later spec once the manifest path is mandatory.
- **`ProviderConfig` is a serde tagged enum** with `#[serde(tag = "type", rename_all = "lowercase")]`. That makes the TOML `[provider] type = "anthropic"` parse into `ProviderConfig::Anthropic { ... }` cleanly.
- **`provider_factory()` reads the env var eagerly.** A missing API key becomes a startup error, not a first-turn error. The only exception is the `fallback = "echo"` path, which gracefully falls back with a stderr warning.
- **Router signature change.** `ConnectionRouter::new(event_tx, api_key, model)` → `ConnectionRouter::new(event_tx, provider_factory)`. The two router tests updated to construct a local `echo_factory()` helper instead of passing `None, "echo".into()`.
- **Verify loop**: `cargo fmt -- --check && cargo clippy && cargo test` all green. kernel-daemon 7 unit tests (was 2, +5 manifest tests: `parses_minimal_manifest`, `rejects_missing_provider_type`, `echo_provider_no_env_var_needed`, `anthropic_fallback_to_echo_when_env_var_missing`, `anthropic_no_fallback_is_hard_error`). All other test suites unchanged.
- **Judge pass and doc-sync skipped per the multi-spec directive** — consolidating doc updates until the whole config-driven arc (0011-0014) lands.
