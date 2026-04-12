---
id: 0014-frontend-via-manifest
status: done
---

# Frontend selected via manifest

## Goal
Close the config-driven arc. The distribution manifest gains a `[frontend]` section with a `type` field; `dist-code-agent::main` reads it and uses it to pick between the TUI and REPL code paths, replacing the current `--repl` CLI flag. Only `"tui"` and `"repl"` are valid values; any other value is a hard error. The default (no `[frontend]` section) remains TUI for backwards compatibility with the `--repl` flag.

After this spec, **all four** of the config-driven concerns (provider, policy, tools, frontend) flow from a single TOML manifest file. The minimum-viable config-driven world is in place.

## Context
- `crates/dist-code-agent/src/main.rs:175` — the `--repl` flag currently picks between `run_repl` and `run_tui`. After this spec, the manifest's `[frontend]` section is the primary signal; `--repl` stays as a backwards-compat override.
- `crates/kernel-interfaces/src/manifest.rs` — where `FrontendConfig` lives.
- `distros/code-agent.toml` — gains a `[frontend]` section.

## Acceptance criteria
- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes
- [ ] `kernel-interfaces::manifest` gains:
    - `pub enum FrontendKind { Tui, Repl }` — serde-lowercase-renamed.
    - `pub struct FrontendConfig { pub kind: FrontendKind }` — the `kind` field deserializes from `type` in the TOML for consistency with `ProviderConfig`. Use `#[serde(rename = "type")]`.
    - `DistributionManifest` grows `pub frontend: Option<FrontendConfig>`.
- [ ] `DistributionSettings` in `dist-code-agent::main` grows a `frontend: FrontendKind` field. The deprecated defaults path returns `FrontendKind::Tui`.
- [ ] `load_distribution_settings` reads `manifest.frontend` and produces the `FrontendKind`. Missing `[frontend]` section defaults to `Tui`.
- [ ] `main()` frontend selection logic:
    1. If `--repl` CLI flag is passed, force REPL mode (override, overrides the manifest). Warn once that `--repl` is legacy and should move to the manifest.
    2. Otherwise use `settings.frontend` from the manifest (or the default `Tui`).
    3. Dispatch to `run_tui(...)` or `run_repl(...)` accordingly.
- [ ] `distros/code-agent.toml` gains:
    ```toml
    [frontend]
    type = "tui"
    ```
- [ ] Two new manifest tests in `kernel-interfaces::manifest::tests`:
    - `parses_frontend_tui` — manifest with `[frontend] type = "tui"` → `FrontendKind::Tui`.
    - `parses_frontend_repl` — manifest with `type = "repl"` → `FrontendKind::Repl`.
- [ ] No test regressions elsewhere.

## Out of scope
- Moving the TUI into a separate `frontend-tui` crate. Spec 0015+ territory.
- New frontend types (web, IDE, headless). Only `tui` and `repl` exist.
- Removing `--repl`. Stays as a compat override.
- Feature-gating the TUI at compile time.
- Frontend-level config (color theme, keybindings). Stays compiled in.

## Notes

- **Config-driven arc complete.** All four concerns — provider, policy, tools, frontend — flow from `distros/code-agent.toml` after this spec.
- **`FrontendConfig` uses `#[serde(rename = "type")]`** so the TOML key is `type` (matching `[provider] type`), while the Rust field is `kind` (which would shadow the keyword otherwise).
- **Override order**: `--repl` CLI flag > manifest `[frontend]` > default `Tui`. The CLI flag wins as a last-resort operator escape hatch; passing it prints a deprecation warning.
- **Daemon test helpers needed `frontend: None` added** to their struct literals. A previous sed produced duplicate fields on the first try; fixed by reading the file and making two exact edits.
- **Verify loop**: `cargo fmt -- --check && cargo clippy && cargo test` all green. kernel-interfaces 30 unit (was 28, +2 new tests `parses_frontend_tui` and `parses_frontend_repl`). No other test counts changed.
- **Judge pass + doc-sync** rolled into the arc-end consolidated pass (next commit).
