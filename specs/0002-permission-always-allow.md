---
id: 0002-permission-always-allow
status: done
---

# Permission prompt: "always allow" key

## Goal
Add an `a` key to the TUI permission prompt that allows the pending tool call **and** hot-swaps the session policy so the same capabilities are auto-allowed for the rest of the session. Implemented purely on existing `KernelRequest::SetPolicy` surface — no protocol changes.

## Context
- `crates/dist-code-agent/src/tui.rs:98-103` — `ConversationEntry::PermissionPrompt { tool_name, capabilities, input_summary }` (the prompt entry the TUI displays)
- `crates/dist-code-agent/src/tui.rs:589-636` — render of the permission prompt, including the `[y/n]` footer that needs to become `[y/n/a]`
- `crates/dist-code-agent/src/tui.rs:779-792` — `InputAction` enum (add `PermissionAlwaysAllow` variant alongside `PermissionDecision`)
- `crates/dist-code-agent/src/tui.rs:826-838` — `handle_key` permission-mode branch that matches `y/Y` and `n/N`; this is where `a/A` handling is added
- `crates/dist-code-agent/src/main.rs:249-269` — where the default policy is constructed inline before `CreateSession`. The current policy value is not kept anywhere after send; this spec needs to stash a mutable copy so `SetPolicy` can update it cumulatively.
- `crates/dist-code-agent/src/main.rs:283-299` — `CreateSession` sends the policy into the daemon
- `crates/dist-code-agent/src/main.rs:470-494` — main-loop dispatch of `InputAction::PermissionDecision(allow)`, which writes `KernelRequest::PermissionResponse`. `PermissionAlwaysAllow` dispatch is added next to this.
- `crates/dist-code-agent/src/main.rs:634-647` — `KernelEvent::PermissionRequired` handler that pushes the `PermissionPrompt` entry and stashes `request_id` in `app.pending_permission_request_id`
- `crates/kernel-interfaces/src/protocol.rs:98-102` — `KernelRequest::SetPolicy { session_id, policy }` (already exists; nothing to add)
- `crates/kernel-interfaces/src/frontend.rs:14-20` — `PermissionRequest { tool_name, capabilities: Vec<String>, input_summary }` — `capabilities` is what "always allow" promotes to a new `Allow` rule
- `crates/kernel-interfaces/src/policy.rs:1-71` — `Policy`, `PolicyRule`, `PolicyAction`, `Policy::evaluate` (first-match-wins, so a prepended `Allow` rule for specific capabilities shadows existing `Ask` rules)
- `crates/kernel-core/src/permission.rs:19` and `crates/kernel-core/src/session.rs:85-86,182-183` — verifies `set_policy` is wired through session → permission evaluator (no kernel-side changes required)
- `docs/roadmap.md` — original G2 roadmap entry (file previously named `docs/tui-roadmap.md`)
- `specs/0001-slash-commands.md` — prior spec in the same TUI area; shows conventions for tui.rs / main.rs work, especially the "parser + unit test" pattern

## Acceptance criteria
- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes
- [ ] The rendered permission-prompt footer reads `[y/n/a]` instead of `[y/n]`
- [ ] In the TUI's permission mode, pressing `a` (or `A`) returns a new `InputAction::PermissionAlwaysAllow` variant from `handle_key`; `y/Y` and `n/N` continue to return `InputAction::PermissionDecision(true|false)` unchanged
- [ ] Main-loop dispatch of `PermissionAlwaysAllow` performs **both** of the following in order, using the pending request's `capabilities`:
    1. Prepends a new `PolicyRule { match_capabilities: <request.capabilities>, action: Allow, scope_paths: [], scope_commands: [], except: [] }` to the dist-side policy copy, then sends `KernelRequest::SetPolicy { session_id: SessionId(0), policy: <updated copy> }`
    2. Sends `KernelRequest::PermissionResponse { request_id, decision: Decision::Allow }` so the *current* pending tool call proceeds
- [ ] After an `a` press, `app.awaiting_permission` is cleared, `app.pending_permission_request_id` is taken, and the `PermissionPrompt` entry is removed from `app.entries` — same cleanup path as the existing `y`/`n` branch
- [ ] A unit test in `tui::tests` verifies that `handle_key` in permission mode returns `PermissionAlwaysAllow` for `KeyCode::Char('a')` and `KeyCode::Char('A')`, and that non-permission-mode key handling is untouched by the change
- [ ] A unit test verifies that prepending an `Allow` rule for a given capability set causes `Policy::evaluate` on that capability to return `Decision::Allow` even when a later rule would `Ask` — i.e., the policy-mutation helper produces a policy that shadows the prior `Ask` rule (first-match-wins is already tested in `policy.rs`, so this test covers the *helper* that builds the updated policy, not the evaluator)
- [ ] The helper that constructs the updated policy lives in `dist-code-agent` (not `kernel-interfaces`) — it's a distribution-level convenience, not a kernel primitive
- [ ] `/status`, `/compact`, `/clear`, `/quit`, y-allow, and n-deny all still work (no regression in spec 0001 behavior)
- [ ] The y/n dispatch branch also clears `pending_permission_capabilities` so the field doesn't outlive its prompt (defensive hygiene — judge-pass adoption; see *Notes*)
- [ ] A second unit test verifies that `prepend_allow_rule` does not affect capabilities outside the promoted set (i.e., unrelated `Ask` rules still evaluate as `Ask`) — judge-pass adoption; see *Notes*

## Out of scope
- Any new `KernelRequest` or `KernelEvent` variants — only `SetPolicy` and `PermissionResponse` are used
- Scoping "always allow" by path, command, or tool name — the new rule matches the request's `capabilities` verbatim. Path/command scoping is a separate feature.
- Persisting "always allow" decisions across sessions (e.g., to a policy file on disk)
- A UI for listing / revoking active "always allow" rules
- A separate "always deny" key
- Changing default policy construction in `main.rs:249-269` beyond what's needed to keep a mutable copy around
- Kernel-side changes to `permission.rs`, `session.rs`, or `event_loop.rs` — `SetPolicy` is already wired end-to-end
- Marking G2 done in `docs/roadmap.md` (then named `tui-roadmap.md`) as a code change (do it as part of the commit, same pattern as spec 0001)
- A `/help` or discoverability affordance for the new `a` key beyond the `[y/n/a]` footer

## Checkpoints
- **After reading context, before writing code**: post a 5-line plan and wait for go/no-go (default first checkpoint)
- **After deciding where the mutable policy copy lives** (App field vs. main-loop local vs. new wrapper): stop and show the shape of that state + the `PermissionAlwaysAllow` dispatch sketch, before wiring the policy-update helper. This is the real architectural seam — the rest of the change is mechanical once this is fixed.

## Notes

- **Policy state location**: settled at checkpoint 2. Kept `current_policy: Policy` as a `let mut` local in `run_tui`, passed `&mut` into `run_tui_loop` as a new parameter. App stays policy-ignorant (the TUI widget should not know about `kernel_interfaces::policy`), and REPL mode doesn't get the mut ref because it has no interactive permission prompt.
- **Default policy extraction**: moved the inline default policy out of `connect_and_setup` into a module-level `default_policy()` helper. `connect_and_setup` now takes `policy: Policy` as a parameter — both `run_tui` and `run_repl` construct via `default_policy()` and pass a clone. This was necessary to give `run_tui` its own mutable copy to update.
- **Capability stash**: added `App::pending_permission_capabilities: Option<Vec<String>>` alongside the existing `pending_permission_request_id`. Stashed at `KernelEvent::PermissionRequired` handling time, taken at dispatch. Symmetric with the existing request_id pattern; avoids reading back the `ConversationEntry::PermissionPrompt` which would couple dispatch to render state.
- **Request to promote**: the new `Allow` rule's `match_capabilities` is the verbatim `PermissionRequest::capabilities` vec — no munging, no prefix matching. "Always allow this" means "always allow the exact capability set of this request".
- **Rule ordering**: inserted at index 0. First-match-wins means the new rule shadows any prior `Ask`/`Deny` on the same capability. Test `prepend_allow_rule_shadows_later_ask` pins this behavior.
- **Ordering of writes in dispatch**: `SetPolicy` is sent *before* `PermissionResponse`. Both go to the kernel on the same connection via the same writer lock, so they're serialized. Doing `SetPolicy` first makes the policy state coherent before the (already-granted) decision is acted on.
- **Y/N branch also clears `pending_permission_capabilities`**: not strictly required for correctness (the field is only read by the always-allow branch), but symmetric with clearing `pending_permission_request_id` and prevents stale state if future code reads it.
- **Unit test location for `prepend_allow_rule`**: lives in a new `#[cfg(test)] mod tests` at the end of `main.rs` (no prior test module there). Covers both the shadowing behavior and non-interference with unrelated capabilities.
- **Verify loop**: `cargo fmt -- --check && cargo clippy && cargo test` all green. dist-code-agent unit tests: 17 (was 12) — +3 tui tests (`permission_mode_a_returns_always_allow`, `permission_mode_y_and_n_unchanged`, `non_permission_mode_a_is_plain_input`) and +2 main tests (`prepend_allow_rule_shadows_later_ask`, `prepend_allow_rule_preserves_other_capabilities`). Integration tests unchanged at 11.
- **Judge pass**: verdict NEEDS ATTENTION on first run. All 9 original ACs MET, no Out-of-scope violations, two scope-creep findings: (a) clearing `pending_permission_capabilities` in the y/n branch (defensive symmetry, not strictly required), (b) the second `prepend_allow_rule_preserves_other_capabilities` test (pins non-interference that AC #7 didn't explicitly ask for). Both kept on judgment: (a) is cheap defense against a future bug where another branch reads a stale capability set, (b) is one extra assert that locks in an invariant we'd regret losing. AC list grew by two bullets to cover them rather than silently adopting — this is the protocol's "scope creep worth keeping → grow the AC" path.
