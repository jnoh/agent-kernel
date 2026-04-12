---
id: 0009-agent-ergonomics
status: draft
---

# Agent Ergonomics — permissions, hooks, orientation, commands, subagents

## Goal
Land the foundational agent-support infrastructure this repo currently lacks: pre-approved safe commands, a pre-commit enforcement hook, an orientation map, canonical slash commands, an extracted doc-sync subagent, and a spec-authoring skill. Together these eliminate routine friction (permission prompts, forgotten verify loops) and make the existing spec-driven workflow easier for a cold agent to follow without changing any runtime behavior in `crates/`.

## Context
- `CLAUDE.md` — current source of truth for rules; the doc-sync prompt will be extracted from here and a small Setup subsection will be added. Read the whole file before touching it.
- `docs/spec-protocol.md` — referenced by the new spec-authoring skill; do not modify.
- `specs/_template.md` — referenced by the new spec-authoring skill; do not modify.
- `.claude/skills/code-quality/SKILL.md` — the only existing skill in the repo; use its frontmatter shape and tone as the reference format for the new skill.
- `docs/architecture.md` — source for the orientation map. Read §1 ("What This Is"), §2 ("Architecture Overview"), and the section headers for §3+ so the map's concept→file table matches reality. Do not copy prose from architecture.md into the map.
- `Cargo.toml` and each `crates/*/Cargo.toml` — confirm the member list, binary names, and production-vs-dev dependency edges so the map is accurate.

## Acceptance criteria

### Permissions and hooks
- [ ] `.claude/settings.json` exists and pre-approves exactly these read/verify commands without prompting: `cargo check`, `cargo test` (any args), `cargo fmt -- --check`, `cargo clippy` (any args), `git status`, `git diff` (any args), `git log` (any args), `git show` (any args), `ls`, `wc -l`. No destructive command (`git push`, `git reset`, `git commit`, `git rebase`, `rm`, `cargo add`, `cargo remove`, `cargo fmt` without `--check`) is pre-approved. Write operations (`Edit`, `Write`) are not pre-approved.
- [ ] `.githooks/pre-commit` exists, is executable (`chmod +x`), starts with `#!/usr/bin/env bash` + `set -euo pipefail`, and runs `cargo fmt -- --check && cargo clippy && cargo test`. Exits non-zero if any step fails. Does **not** call `--no-verify`-bypassable alternatives.
- [ ] `CLAUDE.md` gains a short "Setup" subsection (≤ 8 lines) under "Build / Verify / Test Loop" documenting the one-time `git config core.hooksPath .githooks` command and why (enforces the verify loop CLAUDE.md already requires). No other CLAUDE.md rules move.

### Orientation map
- [ ] `docs/map.md` exists, is **≤ 200 lines**, and contains:
  - A one-paragraph preamble explaining that `map.md` answers "where does X live?" and `architecture.md` answers "why is it this way?" — readers should start here and follow links into `architecture.md` for rationale.
  - For each workspace member (`kernel-interfaces`, `kernel-core`, `kernel-daemon`, `dist-code-agent`): the crate's role in one sentence, its production dependency edges, and a concept→file table mapping every concept named in `architecture.md` §3 to a file path (e.g., "turn loop → `crates/kernel-core/src/turn_loop.rs`").
  - A short "Where non-code lives" section pointing at `specs/`, `policies/`, `docs/`, `.claude/`, and `.githooks/`.
  - No rationale, no architecture arguments, no duplication of prose from `architecture.md`.

### Slash commands
- [ ] `.claude/commands/verify.md` exists. When the user types `/verify`, the agent runs `cargo fmt -- --check && cargo clippy && cargo test` in the repo root and reports a one-line pass/fail plus the failing step on failure. No other behavior.
- [ ] `.claude/commands/review.md` exists. When the user types `/review`, the agent invokes the existing `code-quality` skill against the current working-tree diff (`git diff HEAD`) and emits that skill's report format.
- [ ] `.claude/commands/spec.md` exists. When the user types `/spec <path>`, the agent executes the spec at `<path>` per `docs/spec-protocol.md` §"Executing a spec" (read in full → flip `ready`→`in-progress` → post 5-line plan → stop at first checkpoint). If `<path>` does not exist or is not in `ready` status, the command reports the mismatch and stops.

### Doc-sync subagent extraction
- [ ] `.claude/agents/doc-sync.md` exists and contains the doc-sync prompt currently embedded in `CLAUDE.md`, adapted to the agent-definition frontmatter format (name, description, prompt/system body). The prompt text is **unchanged in meaning** — only formatting and placeholder conventions may change.
- [ ] `CLAUDE.md`'s "Sync docs/ after every spec completion" subsection is rewritten to reference the new subagent by name instead of inlining the full prompt. The sections describing **when** to invoke it and **how** to handle the resulting report remain in `CLAUDE.md` verbatim (those are workflow rules, not the subagent's own instructions).
- [ ] `CLAUDE.md` net line count drops by at least 40 lines as a result of the extraction.

### Spec-authoring skill
- [ ] `.claude/skills/spec-author/SKILL.md` exists. Its frontmatter description triggers on phrases like "write a spec", "turn this into a spec", "draft a spec for", and references `specs/_template.md` as the starting point.
- [ ] The skill body is ≤ 120 lines and does **not** duplicate rules from `docs/spec-protocol.md` — it references the protocol and adds only the operational hints a spec author needs (numbering next spec, reading context before writing, leaving status as `draft`, handing back to the user for approval).

### Verify loop and scope hygiene
- [ ] `cargo fmt -- --check && cargo clippy && cargo test` passes on a clean checkout after all changes.
- [ ] The `.githooks/pre-commit` hook passes when run manually from the repo root (`./.githooks/pre-commit`).
- [ ] `git status` immediately before the commit shows only these paths (plus the spec file itself): `.claude/settings.json`, `.githooks/pre-commit`, `docs/map.md`, `.claude/commands/verify.md`, `.claude/commands/review.md`, `.claude/commands/spec.md`, `.claude/agents/doc-sync.md`, `.claude/skills/spec-author/SKILL.md`, `CLAUDE.md`. Nothing else.

## Out of scope

- Refactoring, renaming, or deleting any file under `crates/`.
- Changing the verify loop itself — it stays `cargo fmt -- --check && cargo clippy && cargo test`.
- Moving, editing, or removing any rule in `CLAUDE.md` other than (a) the doc-sync prompt extraction and (b) the new `Setup` subsection. The rest of `CLAUDE.md` is frozen for this spec.
- CI enforcement for the code-quality skill's hard line caps (tier 3 from prior discussion — defer to its own spec).
- A `SessionStart` hook for Claude Code on the web (tier 3).
- Pruning `crates/kernel-core/src/testutil.rs` or any other tier-3 cleanup.
- Automatic installation of the git hook — one-line documented setup is enough. Do not add a `setup.sh`, `justfile`, `makefile`, or `cargo-husky` dependency.
- Edits to existing specs, to `docs/architecture.md`, to `docs/design-proposals.md`, to `docs/roadmap.md`, to `docs/spec-protocol.md`, or to `specs/_template.md`.
- Edits to `.claude/skills/code-quality/SKILL.md`. It is referenced, not modified.
- New policy files in `policies/`.
- Any change to the workspace `Cargo.toml` or to any crate `Cargo.toml`.

## Checkpoints

- **After reading context, before writing any file**: post a ≤ 5-line plan listing the artifact creation order and which acceptance criterion group each step satisfies. Stop and wait for go/no-go.
- **After creating `.claude/settings.json` and `.githooks/pre-commit`, before creating anything else**: stop and post both files' contents for review. These two files change the risk profile of every subsequent agent action in the repo, so they warrant an explicit checkpoint. Wait for go/no-go before continuing.
- **After extracting the doc-sync prompt and editing `CLAUDE.md`, before creating the spec-author skill**: stop and post the `CLAUDE.md` diff so the user can confirm no unintended rules were moved or reworded.

## Notes

Empty at draft time. Appended during execution.
