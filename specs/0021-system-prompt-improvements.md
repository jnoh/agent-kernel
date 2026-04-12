---
id: 0021-system-prompt-improvements
status: draft
---

# System prompt improvements

## Goal

Tighten the system prompt to make the agent measurably better at
coding tasks. The current prompt has good scope discipline and tool
usage rules but lacks guidance on change validation, efficient file
reading, and error recovery. These gaps lead to common failure modes:
editing without reading, not verifying edits took effect, reading
entire large files when only a section is needed.

## Context

- `crates/agent-kernel/src/prompt/mod.rs` — prompt builder, assembles
  sections with `PromptContext` interpolation.
- `crates/agent-kernel/src/prompt/sections.rs` — the five prompt
  sections: system, doing_tasks, executing_actions, tool_usage,
  git_workflows.

## Design decisions (locked)

**Additions to existing sections, not new sections.** The five-section
structure is good. We add guidance within them:

1. **Tool usage section** — add:
   - "Always read a file before editing it. file_edit will fail if
     old_string doesn't match."
   - "For large files, use offset + limit to read just the relevant
     section. Don't read 5000 lines to edit line 42."
   - "After a file_edit, read the edited region to confirm the change
     landed correctly."
   - "Use grep to find the right file before reading. Don't guess paths."

2. **Doing tasks section** — add:
   - "When a tool call fails, read the error message. Diagnose before
     retrying. Don't repeat the same failing call."
   - "If the model's output was truncated or a tool result is unclear,
     ask for clarification rather than guessing."
   - "After making changes, run the project's test/build command if one
     exists to verify nothing broke."

3. **Executing actions section** — add:
   - "If a shell command fails, check exit code and stderr before
     retrying with a different approach."

**No behavioral changes to the runtime.** This is a prompt-only spec.
The tool implementations, turn loop, and TUI are untouched.

## Acceptance criteria

- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes.
- [ ] `sections.rs` updated with the new guidance text.
- [ ] The additions are integrated naturally into existing section
      prose, not appended as a disconnected list.
- [ ] `build_system_prompt` test still passes.
- [ ] No changes outside `crates/agent-kernel/src/prompt/`.

## Out of scope

- Prompt caching strategy changes.
- Dynamic prompt content (tool-specific instructions based on which
  tools are loaded).
- Changes to the turn loop or tool dispatch.
- Benchmarking prompt quality.

## Checkpoints

Standing directive: skip checkpoints, execute to completion.

## Notes

Empty at draft time.
