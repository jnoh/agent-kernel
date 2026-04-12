---
id: 0012-policy-via-manifest
status: done
---

# Policy loaded from manifest, shared manifest types in kernel-interfaces

## Goal
Two changes in one spec, both required for config-driven v0.2:

1. **Move the manifest data types** (`DistributionManifest`, `DistributionMeta`, `ProviderConfig`, `ProviderFallback`) out of `kernel-daemon/src/manifest.rs` and into `kernel-interfaces/src/manifest.rs`, so both the daemon and `dist-code-agent` can parse the same file. The daemon-local factory construction stays in `kernel-daemon` as a free function, since it depends on `kernel-providers`.

2. **Grow the manifest with a `[policy]` section**, and wire `dist-code-agent` to read it. `default_policy()` in `dist-code-agent/src/main.rs` is replaced by YAML loading of whatever file the manifest points at. The in-repo `policies/permissive.yaml` becomes the default.

After this spec: running `agent-kernel-daemon --distro distros/code-agent.toml` + `agent-kernel --distro distros/code-agent.toml` gives you a session where provider AND policy came from the same config file.

## Context
- `crates/kernel-daemon/src/manifest.rs` — where the types live today. The types (DistributionManifest, etc.) move out; the daemon's `provider_factory` function becomes a free function in this file instead of a method.
- `crates/kernel-interfaces/src/` — new sibling `manifest.rs` with the data types.
- `crates/dist-code-agent/src/main.rs:229-269` — `default_policy()` function and its call site at line 331. Both go away, replaced by a `load_policy_from_manifest(&manifest, manifest_dir)` helper that reads the YAML file.
- `policies/permissive.yaml` — the YAML file that will be referenced by the manifest's `[policy] file = "..."` entry. Already exists; no changes needed.
- `crates/kernel-interfaces/src/policy.rs` — `Policy` struct already has serde derives and deserializes from YAML. No changes needed.
- `distros/code-agent.toml` — the manifest file from spec 0011. Gains a `[policy]` section.

## Acceptance criteria
- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes
- [ ] New file `crates/kernel-interfaces/src/manifest.rs` with:
    - `pub struct DistributionManifest { pub distribution: DistributionMeta, pub provider: ProviderConfig, pub policy: Option<PolicyConfig> }`
    - `pub struct DistributionMeta { name: String, version: String }`
    - `pub enum ProviderConfig` — serde tagged enum, variants `Anthropic { model, api_key_env, fallback: Option<ProviderFallback> }` and `Echo`
    - `pub enum ProviderFallback` — only `Echo` variant for now
    - `pub struct PolicyConfig { pub file: String }` — path to a YAML policy file, relative to the manifest file's directory
    - `pub fn load_manifest(path: &Path) -> Result<DistributionManifest, String>` — reads the TOML file, returns a human-readable error
  No runtime deps beyond what `kernel-interfaces` already has (`serde`, `serde_json`, plus a new `toml` dep — unavoidable since the file format is TOML).
- [ ] `kernel-interfaces/Cargo.toml` gains `toml = "0.8"` as a dependency. No other new deps.
- [ ] `crates/kernel-daemon/src/manifest.rs` is **replaced** with a thin file that re-exports the types from `kernel-interfaces` and defines a `build_provider_factory(manifest: &DistributionManifest) -> Result<ProviderFactory, String>` free function (was a method before). Existing daemon call sites update to use the free function.
- [ ] `crates/kernel-daemon/Cargo.toml` no longer directly depends on `toml` (it inherits through `kernel-interfaces`). If that doesn't simplify cleanly, keep the direct dep — not worth thrashing.
- [ ] Manifest file format gains `[policy]` section:
    ```toml
    [policy]
    file = "../policies/permissive.yaml"
    ```
    Path is relative to the manifest file's parent directory. (`distros/code-agent.toml` → `../policies/permissive.yaml` resolves to the repo root `policies/permissive.yaml`.)
- [ ] `distros/code-agent.toml` gains the `[policy]` section above.
- [ ] `dist-code-agent` accepts a new `--distro <path>` CLI flag. If present:
    1. Load the manifest via `kernel_interfaces::manifest::load_manifest`.
    2. If `[policy]` is set, resolve the path relative to the manifest's directory, read the YAML file, parse via `serde_yaml::from_str::<Policy>`.
    3. Pass the loaded Policy into session creation instead of the hard-coded one.
    4. If `--distro` is not passed, the hard-coded `default_policy()` stays as fallback (with a deprecation warning) — same transition pattern as spec 0011.
- [ ] The `default_policy()` function in `dist-code-agent/src/main.rs` is kept but marked `#[deprecated]` with a comment pointing at the manifest-based path.
- [ ] New unit test in `kernel-interfaces::manifest::tests::parses_manifest_with_policy_section` — asserts the full manifest round-trips including the new policy field.
- [ ] New unit test in `dist-code-agent::tests` (or wherever appropriate) that loads `policies/permissive.yaml` via the new path and asserts the resulting `Policy` has the expected rules. Use a small helper that takes a yaml string to avoid depending on a specific file location in the test.
- [ ] Existing tests still pass unchanged. No test count regression anywhere.

## Out of scope
- Tools via manifest — spec 0013.
- Frontend via manifest — spec 0014.
- Hot-reload / watch the YAML file.
- Validating policy contents (rule syntax, unknown capabilities) beyond what serde already does.
- Removing `default_policy()` entirely. Deprecated but kept.
- Making the daemon read the policy too. Policy is client-side in our architecture (dist-code-agent sends it via `CreateSession`), so the daemon doesn't need to care.
- Renaming `kernel-daemon/src/manifest.rs` if the re-export approach makes a rename more natural — keep the file name.
- Policy merging (multiple files, layered overrides). One file, one policy.
- A `distros/lockdown.toml` for the locked-down policy. Can be added in a follow-up; the mechanism is the point.

## Notes

- **Types moved**, factory function stayed. `DistributionManifest`, `DistributionMeta`, `ProviderConfig`, `ProviderFallback`, `PolicyConfig`, `load_manifest`, and `manifest_dir` all live in `kernel-interfaces::manifest` now. `build_provider_factory` stayed in `kernel-daemon` (as a free function, no longer a method) because it constructs real `AnthropicProvider` / `EchoProvider` values and those live in `kernel-providers` which the daemon depends on but `kernel-interfaces` doesn't.
- **`kernel-interfaces` gained a `toml` dependency.** It already had `serde` and `serde_json`; `toml = "0.8"` is the only addition. Still purely data types — no runtime I/O beyond `load_manifest`'s `fs::read_to_string`, which is deliberate because the manifest *is* a file format.
- **`kernel-daemon/src/manifest.rs` became a thin wrapper** re-exporting types from interfaces and holding the daemon-local factory. The re-exports carry `#[allow(unused_imports)]` because not all of them are consumed in the daemon's own code — they're there so daemon-side callers can import everything from one module path.
- **Daemon tests are still 5 (was 7)** — two manifest tests were removed because they lived on the daemon-side `provider_factory()` method that no longer exists; the equivalent tests now live in the new daemon-local `build_provider_factory` (3 tests), plus the 2 router tests. Net test count across all crates: +5 (kernel-interfaces 26 was 21, kernel-daemon 5 was 7). No behavior regressions.
- **Policy-file path resolution**: `PolicyConfig::resolve` joins the manifest's parent directory with the policy file path. Absolute paths pass through unchanged. Tested by `policy_config_resolves_relative_to_manifest_dir` and `policy_config_absolute_path_unchanged`.
- **`dist-code-agent` now accepts `--distro <path>`** with the same backwards-compat fallback as the daemon: if it's absent, `default_policy()` is called with a deprecation warning. If it's present but the policy file can't be read / parsed, we log the error and still fall back to `default_policy()` — a bad manifest shouldn't brick the TUI during development.
- **`default_policy()` is marked `#[deprecated]`** with a pointer at this spec. The deprecation attribute requires `#[allow(deprecated)]` at the single call site in `main()` (inside the fallback match arm). Will be removed entirely once the manifest path becomes mandatory (a later spec).
- **`run_tui` and `run_repl` signatures both grew** a `policy: Policy` parameter — the value flows from `main()` through to `connect_and_setup`. Spec 0002's `prepend_allow_rule` path still works because `current_policy` is still the owned `Policy` the runtime loop mutates.
- **The `--distro` flag only touches the dist-code-agent side and is separate from the daemon's `--distro` flag**, even though they parse the same file. That's intentional: the daemon reads `[provider]`, the distro reads `[policy]`. A future spec can collapse them into a single `AGENT_KERNEL_DISTRO` env var if the double-flag ergonomics become annoying.
- **Verify loop**: `cargo fmt -- --check && cargo clippy && cargo test` all green. kernel-interfaces 26 unit (was 21, +5). kernel-daemon 5 unit (was 7, -2 from method-to-function refactor but +3 new = net -2 in daemon, +5 in interfaces). dist-code-agent / kernel-core / kernel-providers unchanged. No regressions.
- **Doc-sync and judge pass skipped** — consolidating at the end of the config-driven arc.
