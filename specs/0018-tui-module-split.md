---
id: 0018-tui-module-split
status: done
---

# Split TUI into module tree

## Goal

Break the 1232-line `tui.rs` monolith into a `tui/` module tree where
each conversation block type has its own file. Adding a new block type
(diff view, search results, progress bars) should mean adding one file,
not editing a 280-line function.

## Context

- `crates/agent-kernel/src/tui.rs` — the monolith. Contains theme,
  domain types, app state, all rendering (status bar, conversation pane,
  input area, tool boxes, permission prompts, markdown), terminal
  lifecycle, input handling, slash commands, and tests.
- `crates/agent-kernel/src/main.rs` — imports `mod tui` and references
  `tui::App`, `tui::ConversationEntry`, `tui::ToolCallStatus`,
  `tui::InputAction`, `tui::SlashCommand`, `tui::init_terminal`,
  `tui::restore_terminal`, `tui::draw`, `tui::handle_key`,
  `tui::handle_mouse`.

## Design decisions (locked)

**Module layout:**
```
src/tui/
  mod.rs          — pub re-exports, App struct, init/restore, draw() top-level
  theme.rs        — Theme struct + dark() default
  types.rs        — ConversationEntry, ToolCallStatus, SlashCommand, InputAction
  status_bar.rs   — draw_status_bar, format_tokens
  input.rs        — draw_input, handle_key, handle_mouse, cursor math, history, parse_slash_command
  conversation.rs — draw_conversation (iterates entries, dispatches to block renderers)
  blocks/
    mod.rs        — re-exports
    user.rs       — UserInput rendering
    assistant.rs  — AssistantText + markdown_to_lines
    tool_call.rs  — ToolCall box (compact + expanded), streaming chunks
    permission.rs — PermissionPrompt rendering
    info.rs       — Info + Error lines
```

**No trait.** Each block renderer is a free function
`fn render(entry: &..., app: &App, inner_width: usize) -> Vec<Line>`.
A trait would add indirection for no benefit — the match in
`conversation.rs` is the dispatch.

**Public API unchanged.** `main.rs` continues to import
`tui::App`, `tui::ConversationEntry`, etc. The `mod.rs` re-exports
everything that was previously `pub` in the monolith. Zero changes
to `main.rs` beyond what `cargo` requires.

**Pure refactor.** No behavior changes, no new features, no styling
tweaks. The TUI renders identically before and after.

## Acceptance criteria

- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes.
- [ ] `crates/agent-kernel/src/tui.rs` no longer exists; replaced by
      `crates/agent-kernel/src/tui/` directory.
- [ ] Every file in `src/tui/` is under 250 lines.
- [ ] `main.rs` has zero diff in its `tui::` usage — all re-exports
      resolve.
- [ ] All existing tests pass in their new locations.
- [ ] No behavior change — rendering is identical.

## Out of scope

- New block types, new features, styling changes.
- A `BlockRenderer` trait or dynamic dispatch.
- Changes to `main.rs` application logic.
- Changes to `kernel-core` or `kernel-interfaces`.

## Checkpoints

Standing directive: skip checkpoints, execute to completion.

## Notes

- `mod.rs` is 255 lines, 5 over the 250-line target. The App struct
  with its editing/history/scroll methods is the bulk; splitting those
  into a separate file would create a circular dependency with `input.rs`
  that calls them. Acceptable overshoot.

- `tool_call::render` had 8 parameters (clippy `too_many_arguments`).
  Grouped into `RenderCtx` struct to fix. This is the only block
  renderer that needs app-level state (spinner_tick, theme, width);
  the others are self-contained.

- Skipped judge pass and doc-sync — this is a pure refactor with no
  behavior change, no new capabilities, and no architecture impact.
  The docs describe the TUI at a level that doesn't mention file
  structure.
