---
id: 0013-tools-via-manifest
status: done
---

# Tools selected via manifest

## Goal
The distribution manifest gains a `[tools]` section declaring which tool IDs to enable. `dist-code-agent::tools::create_tools` stops returning "every tool the distro implements" and instead filters to the set the manifest names. A missing `[tools]` section means "enable every first-party tool" for backwards compatibility.

This is intentionally the **minimum-viable** config-driven tool story: the six first-party tools stay compiled into `dist-code-agent`, and the manifest only picks a subset. The architecture doc's full three-path tool vision (native feature / external JSON-RPC process / MCP bridge) is deliberately out of scope — none of those paths exist in code, and building them is a much larger arc. Picking-among-compiled-tools is the smallest step that still moves us from "tools hardcoded in `create_tools`" to "tools declared in the manifest."

## Context
- `crates/dist-code-agent/src/tools.rs:594-603` — `create_tools` returns a hard-coded 6-tool Vec. After this spec it takes a `&[String]` of enabled tool IDs (or `None` to mean "all") and filters.
- `crates/kernel-interfaces/src/manifest.rs` — where the new `ToolsConfig` type lives.
- `crates/dist-code-agent/src/main.rs:331` — where `create_tools(workspace)` is called. After this spec it's called with the manifest-derived list.
- `distros/code-agent.toml` — gains a `[tools]` section.

## Acceptance criteria
- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes
- [ ] `kernel-interfaces::manifest` gains:
    - `pub struct ToolsConfig { pub enabled: Vec<String> }` — a list of tool IDs to enable.
    - `DistributionManifest` grows `pub tools: Option<ToolsConfig>`. `None` = "enable every tool the distribution implements" (backwards compat).
- [ ] `dist-code-agent::tools::create_tools` signature changes to `create_tools(workspace: &Path, enabled: Option<&[String]>) -> Vec<Box<dyn ToolRegistration>>`. Semantics:
    - `None` → return all six tools (current behavior).
    - `Some(ids)` → return only the tools whose `.name()` appears in `ids`. Unknown IDs are logged to stderr as warnings but don't error.
- [ ] Each tool in `tools.rs` has a known tool ID (the same string its `name()` method returns). Add a module-level `pub const TOOL_IDS: &[&str] = &["file_read", ...];` so callers can enumerate available IDs for error messages. Check actual tool names by reading the existing `name()` impls.
- [ ] `distros/code-agent.toml` gains:
    ```toml
    [tools]
    enabled = ["file_read", "file_write", "file_edit", "shell", "ls", "grep"]
    ```
    Listing every first-party tool — the default enables everything.
- [ ] `dist-code-agent::main` reads the manifest's `tools.enabled` list (if present) and passes it into `create_tools`. If no `--distro` or no `[tools]` section, calls `create_tools(workspace, None)` (all tools).
- [ ] Both `run_tui` and `run_repl` call `create_tools` with the same filter. Since both functions already take `workspace`, they also now take the enabled list.
- [ ] Two new manifest tests in `kernel-interfaces::manifest::tests`:
    - `parses_manifest_with_tools_section` — asserts `tools.enabled` parses correctly.
    - `missing_tools_section_is_none` — manifest without `[tools]` parses with `tools = None`.
- [ ] Existing `tools_test.rs` integration tests pass unchanged; they construct tools directly without going through `create_tools`, so they're insulated from the signature change. If any do call `create_tools`, update them to pass `None`.
- [ ] New unit test in `dist-code-agent` (maybe a new `tools::tests` module if none exists) verifying `create_tools(ws, Some(&["file_read", "grep"]))` returns exactly 2 tools with those names.

## Out of scope
- External tools (JSON-RPC stdin/stdout) — the second of the three tool paths. Requires process spawning, manifest parsing, and a new abstraction layer. Big spec on its own.
- MCP bridge — the third path. Ditto.
- Per-tool configuration (e.g., `file_read.max_size_bytes = 1_000_000`). Tool config stays compiled in.
- Policy-based tool filtering (e.g., "the policy says `shell:exec = deny`, so don't even load the shell tool"). Policy gates at dispatch time; loading is separate.
- Adding new first-party tools. The existing six stay; the point is config-driven selection, not ecosystem growth.
- Removing tools from `tools.rs`. Keeping all six so the default stays fully featured.
- A `[tools.mcp]` or `[tools.external]` subsection — those come later.

## Notes

- **`DistributionSettings` bundled struct.** The main() function now loads one `DistributionSettings { policy, enabled_tools }` from the manifest rather than threading two separate values. Cleaner entry point for future manifest fields (frontend, skills, etc.) — each one adds a field to the struct, not a new parameter to `run_tui` / `run_repl`. `run_tui` and `run_repl` take the whole struct by value and destructure as needed.
- **`load_policy_from_manifest` removed**; replaced by `load_distribution_settings` which does both policy and tool loading in one pass. The latter returns an error if the manifest has no `[policy]` section — spec 0012 made policy mandatory in the manifest path.
- **`create_tools(workspace, enabled: Option<&[String]>)`** is the new signature. `None` = all tools; `Some(&[])` = no tools; `Some(ids)` = filter to matching names. Unknown IDs log a warning and are silently dropped. Four new tests (`none_enables_every_tool`, `empty_list_disables_everything`, `filter_narrows_to_named_tools`, `unknown_id_is_warned_and_dropped`).
- **`TOOL_IDS` constant** in `dist-code-agent::tools` lists the six compiled-in tool names. Used only for the unknown-ID warning message today, but future tool-discovery / help-text code can enumerate from it.
- **Three `create_tools` call sites updated** in `main.rs`: `run_tui`'s initial construction (line 415), `run_tui`'s reader-thread tools (line 426), `run_repl`'s reader-thread tools (line 871). Both reader threads now filter identically to the main thread because they share the `enabled_tools: Option<Vec<String>>` local that lives on the stack.
- **Daemon tests** (`kernel-daemon/src/manifest.rs`) needed `tools: None` added to two test helper functions — `DistributionManifest` grew a field and Rust's strict struct-literal check required them. Fixed via `replace_all` on the first one and a direct edit on the second.
- **Verify loop**: `cargo fmt -- --check && cargo clippy && cargo test` all green. Test counts: kernel-interfaces 28 (was 26, +2 for the new tools-section tests), dist-code-agent 21 unit (was 17, +4 from `filter_tests`). All other suites unchanged. No behavior regressions on the existing test paths.
- **Judge pass + doc-sync skipped** — consolidated at end of arc.
