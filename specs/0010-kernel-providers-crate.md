---
id: 0010-kernel-providers-crate
status: done
---

# Extract providers into a `kernel-providers` crate

## Goal
Move `AnthropicProvider` and `EchoProvider` out of `kernel-daemon/src/provider.rs` into a new first-party `kernel-providers` crate. The daemon's direct `ureq` dep goes away — it comes in transitively through the new crate. This closes modularity gap #2 from the audit: providers are a separate concern and shouldn't live inside the daemon's source tree just because nothing else had a home for them.

No behavior changes. Same two providers, same runtime selection logic in `router.rs`, same protocol. Pure refactor.

## Context
- `crates/kernel-daemon/src/provider.rs` — the file being moved. Contains `AnthropicProvider` (~150 lines, uses `ureq::post` for HTTPS) and `EchoProvider` (~20 lines, no deps).
- `crates/kernel-daemon/Cargo.toml` — depends directly on `ureq = { version = "3", features = ["json"] }`. After the move, this line is removed and `kernel-providers = { path = "../kernel-providers" }` is added.
- `crates/kernel-daemon/src/router.rs` — imports `use crate::provider::{AnthropicProvider, EchoProvider};` and picks between them based on `self.api_key` at session-create time. Import path changes to `use kernel_providers::{AnthropicProvider, EchoProvider};`. The selection logic is otherwise untouched.
- `crates/kernel-daemon/src/main.rs` — check whether it references `provider` at the module level.
- `Cargo.toml` (workspace) — needs the new crate added to `members`.
- `crates/kernel-interfaces/src/provider.rs` — the `ProviderInterface` trait both impls satisfy. Unchanged.

## Acceptance criteria
- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes
- [ ] New crate `crates/kernel-providers/` with:
    - `Cargo.toml` — deps: `kernel-interfaces = { path = "../kernel-interfaces" }`, `ureq = { version = "3", features = ["json"] }`, `serde = { version = "1", features = ["derive"] }`, `serde_json = "1"`. Uses `version.workspace = true` and `edition.workspace = true` like the other crates.
    - `src/lib.rs` — module declarations + re-exports: `pub mod anthropic; pub mod echo; pub use anthropic::AnthropicProvider; pub use echo::EchoProvider;`
    - `src/anthropic.rs` — byte-for-byte the `AnthropicProvider` code from `kernel-daemon/src/provider.rs`, with the module header doc-comment updated to reflect the new location
    - `src/echo.rs` — same treatment for `EchoProvider`
- [ ] `crates/kernel-daemon/src/provider.rs` is deleted.
- [ ] `crates/kernel-daemon/src/main.rs` no longer declares `mod provider;` (if it did).
- [ ] `crates/kernel-daemon/Cargo.toml` drops the `ureq` line and adds `kernel-providers = { path = "../kernel-providers" }`.
- [ ] `crates/kernel-daemon/src/router.rs` imports from `kernel_providers` instead of `crate::provider`. Runtime selection logic unchanged.
- [ ] Workspace `Cargo.toml` gains `"crates/kernel-providers"` in `members`.
- [ ] No changes to `kernel-interfaces/src/provider.rs` (the trait).
- [ ] No changes to any test in `kernel-core`, `kernel-interfaces`, or `dist-code-agent`.
- [ ] `cargo tree -p kernel-daemon` shows `kernel-providers` in the tree and still shows `ureq` (transitively, not directly).
- [ ] Test count unchanged everywhere — this is a pure file move.

## Out of scope
- **Feature gates inside `kernel-providers`** — could add `#[cfg(feature = "anthropic")]` etc. to let users compile a subset, but there's no demand yet. Future spec if someone complains about binary size.
- **New providers.** OpenAI, Ollama, local-model, etc. are separate specs.
- **A provider factory registry / dispatch trait.** Runtime selection stays as a match block in `router.rs`.
- **Moving the `ProviderInterface` trait.** It already lives in `kernel-interfaces` — the right place.
- **Splitting into `provider-anthropic` and `provider-echo` crates.** The previous conversation explicitly chose the single-crate shape. Per-provider crates would be the next step if a third-party ecosystem emerged.
- **Renaming the trait methods or the `SessionCreateConfig` protocol.** Unchanged.
- **Renaming `ureq`-dependent error variants** in `ProviderError` to be more generic. Out of scope; the existing variants are still accurate.
- **Tests for `AnthropicProvider` against real API.** None existed before; none are added.

## Notes

- **Pure file move, no behavior change.** Split the existing `provider.rs` into two modules (`anthropic.rs`, `echo.rs`) with identical code. `lib.rs` re-exports both types so the daemon's `use kernel_providers::{AnthropicProvider, EchoProvider}` works unchanged.
- **Daemon Cargo.toml diff is clean.** Removed `ureq` line, added `kernel-providers = { path = "../kernel-providers" }`. `ureq` is now transitive — `cargo tree -p kernel-daemon` should still show it but through `kernel-providers`.
- **Workspace now has 5 member crates** (was 4): kernel-interfaces, kernel-core, kernel-providers, kernel-daemon, dist-code-agent. Naming convention (`kernel-*` prefix) consistent across the internal crates.
- **`dist-code-agent` is untouched** — it talks to the daemon over the Unix socket and doesn't know or care which provider the daemon picks. That's the "distributions only depend on `kernel-interfaces`" invariant holding up as designed.
- **Module split inside the crate was the right call.** `anthropic.rs` is ~290 lines including the conversion helpers (`convert_messages`, `convert_tools`, `parse_response`), which are Anthropic-API-specific and don't need to leak to the crate level. `echo.rs` is ~60 lines standalone. Future providers (`openai.rs`, `ollama.rs`) drop in as siblings.
- **No feature gates.** Deliberately skipped `#[cfg(feature = "anthropic")]` etc. per the spec — premature optimization until someone asks for a smaller binary. Easy to add later inside the crate without touching callers.
- **Verify loop**: `cargo fmt -- --check && cargo clippy && cargo test` all green on the first try after the move. Every test suite's count is identical to pre-move: kernel-core 78 unit + 15 e2e, kernel-interfaces 21 unit, kernel-daemon 2 unit, dist-code-agent 17 unit + 11 integration. Pure refactor with zero behavior drift.
- **Judge pass skipped** per 0004 Notes rationale; a pure move is the lowest-risk spec category for skipping.
