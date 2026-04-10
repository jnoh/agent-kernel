---
id: NNNN-short-name
status: draft  # draft | ready | in-progress | done
---

# {{Title}}

## Goal
One or two sentences. What changes about the system when this is done.

## Context
Files Claude should read before planning. Include line ranges when useful.
- `crates/foo/src/bar.rs` — relevant module
- `docs/agent-kernel-v01-spec.md:120-180` — design background
- Related prior work, commits, or specs

## Acceptance criteria
Concrete, checkable. The verify loop should be one of these.
- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes
- [ ] New test `path::to::test_name` exists and passes
- [ ] Behavior X is observable (describe how to confirm)
- [ ] No changes to public API of `kernel-interfaces` (or: API change is documented in spec)

## Out of scope
Explicit fences. Anything tempting-but-unrelated goes here so Claude doesn't drift.
- Refactoring unrelated modules
- Adding configuration knobs not required by the goal
- Documentation beyond what acceptance criteria require

## Checkpoints
Points where Claude must stop and report instead of continuing.
- After reading context and before writing code: post a 5-line plan and wait for go/no-go
- (Add task-specific checkpoints, e.g., "after defining the trait, before wiring implementations")

## Notes
Empty at draft time. Claude appends findings, blockers, and decisions here during execution.
