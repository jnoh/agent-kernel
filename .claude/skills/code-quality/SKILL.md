---
name: code-quality
description: Review agent-kernel code for layered-architecture violations and right-sized modules before changes land. Use when adding new files, moving code between crates, introducing a new dependency edge, splitting or growing a module, or before committing a non-trivial change. Also use when the user asks about "code quality", "architecture review", "module boundaries", or "file is too big".
---

# Code Quality Skill — agent-kernel

Two things matter for this repo: **layering stays clean** and **modules stay small enough that changes are isolated**. Everything below serves those two goals.

## When to run this skill

Run it before committing any change that:

- Adds a new `.rs` file or grows an existing one past the soft cap below.
- Adds or changes a `[dependencies]` entry in a `Cargo.toml` under `crates/`.
- Moves code between `kernel-interfaces`, `kernel-core`, `kernel-daemon`, or `dist-code-agent`.
- Introduces a new trait, a new public type, or a new `pub use` re-export.
- Touches more than ~3 files in a single logical change (signal that the cut is probably wrong).

If none of those apply, you don't need this skill — skip it.

## 1. The layering rule (hard)

The crate graph is a DAG. Memorize it and do not violate it:

```
kernel-interfaces  (leaf — traits + shared types only)
       ↑
kernel-core        (implements the kernel; depends only on kernel-interfaces)
       ↑                ↑
kernel-daemon      dist-code-agent
(binary, bridges core  (reference distribution; production code depends
 and interfaces)        ONLY on kernel-interfaces — kernel-core is
                        dev-dependency for integration tests)
```

### Invariants to enforce

1. **`kernel-interfaces` is a leaf.** It never imports another workspace crate. It holds `trait` definitions, enums, and data types. Any function body longer than a trivial constructor/validator is a smell — the logic probably belongs in `kernel-core` or a distribution.
2. **`kernel-core` depends only on `kernel-interfaces`.** No reaching sideways to `dist-code-agent` or `kernel-daemon`. Core must be consumable by any distribution.
3. **`dist-code-agent` production code depends only on `kernel-interfaces`.** `kernel-core` is allowed as a `dev-dependency` for integration tests, not for runtime code. If a distribution needs something from core, it means either (a) the thing should have been in `kernel-interfaces` behind a trait, or (b) the distribution is implementing it wrong. Stop and fix the abstraction rather than adding a runtime dep on core.
4. **Traits go in `kernel-interfaces`. Implementations go in the consumer crate.** If a new trait is being defined inside `kernel-core` or a dist crate and is intended for external implementers, it's in the wrong place.
5. **No `pub use` across crate boundaries for convenience.** Re-exports are fine within a crate, but do not launder types from `kernel-core` back out through `kernel-interfaces` or vice versa — that silently couples layers.

### How to check

Run from the repo root:

```bash
# 1. Confirm the dependency graph hasn't grown new edges.
for f in crates/*/Cargo.toml; do echo "=== $f ==="; grep -A1 '^\[dependencies\]\|^\[dev-dependencies\]' "$f"; done

# 2. Verify kernel-interfaces has no workspace deps.
grep -E 'path *= *"\.\./' crates/kernel-interfaces/Cargo.toml && echo "VIOLATION: kernel-interfaces must be a leaf" || echo "OK"

# 3. Verify dist-code-agent production code does not import kernel_core.
grep -rn 'use kernel_core\|kernel_core::' crates/dist-code-agent/src/ && echo "VIOLATION: dist-code-agent production code must not depend on kernel-core" || echo "OK"
```

If any check fires a VIOLATION, stop and redesign. Do not merge a layering break — it's exactly the kind of drift that makes future changes non-isolated.

## 2. File and module sizing

Right-sized files are the mechanism by which changes stay isolated. The goal is not a line count — it's that when someone edits a feature, they touch one file, maybe two. When you have to open five files and hold them all in your head, the cut is wrong.

### Soft targets

| Metric                    | Soft cap | Hard cap | Action at hard cap                              |
|---------------------------|---------:|---------:|-------------------------------------------------|
| Lines in a single `.rs`   |      500 |      800 | Plan a split this commit or file a follow-up.   |
| Public items per module   |       15 |       25 | Some should be `pub(crate)` or moved.           |
| `impl` block length       |      200 |      400 | Extract private helpers into a sibling module.  |
| `fn` length               |       60 |      120 | Factor out helpers; the fn is doing too much.   |
| `match` arms in one fn    |       10 |       20 | Dispatch table, sub-function per arm, or enum.  |

Soft cap = yellow flag, mention it and consider splitting. Hard cap = red flag, do not let a new commit take a file over the hard cap without either splitting it in the same commit or recording a tracked follow-up.

The existing repo already has files over the hard cap (e.g., `kernel-core/src/context.rs`, `dist-code-agent/src/tui.rs`, `kernel-core/src/session_events.rs`). **Do not grow them further.** If you need to add code to one of those files, split first, then add.

### Good cuts vs. bad cuts

A **good cut** produces modules that:

- Have one reason to change (one feature, one subsystem, one protocol version).
- Expose a narrow surface (< ~10 `pub` items per module is a reasonable goal).
- Can be understood without opening their siblings.
- Match a concept named in `docs/architecture.md` — turn loop, context manager, permission evaluator, etc. If you can't name the module from the architecture doc, you may be inventing a new subsystem without documenting it.

A **bad cut** produces modules that:

- Are defined by mechanics ("helpers", "utils", "misc", "common"). These become grab bags and grow without bound.
- Share mutable state across the cut via `pub(crate)` fields. That's a fiction of a boundary.
- Force every caller to import from two modules at once — the cut is in the wrong place.
- Are < ~30 lines and used by exactly one other module. Inline it.

### Checking file sizes

```bash
find crates -type f -name "*.rs" -not -path "*/target/*" \
  | xargs wc -l \
  | sort -rn \
  | awk '$1 > 500 { printf "%-6s %s\n", $1, $2 }'
```

Anything listed above 800 is already over the hard cap — do not add to it in this commit.

## 3. Review procedure

When you're asked to review a change (yours or the user's) against this skill, work through these steps in order. Stop and report at the first failing step.

### Step 1 — Inspect the diff

```bash
git status
git diff --stat HEAD
git diff HEAD -- 'crates/**/Cargo.toml'
```

Note: which crates are touched, which files grew, which dependency sections changed.

### Step 2 — Layering check

Run the three checks from §1. If any VIOLATION fires, stop — report it to the user with a proposed fix (usually: "this trait belongs in `kernel-interfaces`" or "move this impl into the calling crate").

### Step 3 — Sizing check

Run the file-size check from §2. For each file the diff grew:

- Record old size → new size.
- If new size crosses a soft cap that was previously under, mention it but allow.
- If new size crosses the hard cap, stop — propose a split point.

### Step 4 — Module boundary sniff test

For each new or substantially modified module, answer:

1. What is its **one** reason to change? Write it in one sentence. If the sentence needs "and", the module is doing two things.
2. What is its public surface? List the `pub fn`, `pub struct`, `pub trait`. If > 15 items, suggest demoting some to `pub(crate)` or splitting.
3. Does the module name appear in `docs/architecture.md`? If not, is this a genuinely new subsystem that needs to be documented there (see the "Sync docs/" rule in `CLAUDE.md`), or is it a grab-bag that should be folded into an existing module?

### Step 5 — Changeset isolation check

Look at the list of touched files as a whole. Ask:

- Does this change touch **exactly** the files you'd expect from its description? If the description says "add a tool" but the diff touches 7 files across 3 crates, the cut is leaking.
- Are there incidental edits (renames, formatting, unrelated cleanups) bundled in? If yes, tell the user and offer to split into separate commits per `CLAUDE.md`'s commit-hygiene rule.

### Step 6 — Standard verify loop

Per `CLAUDE.md`:

```bash
cargo fmt -- --check && cargo clippy && cargo test
```

Clippy warnings count as quality issues — do not dismiss them. If a lint is genuinely wrong for a spot, `#[allow(...)]` with a one-line comment explaining why, not a blanket crate-level allow.

## 4. Reporting

When this skill runs, produce a short report in this shape (no preamble, no restating the rules):

```
Layering: ok | VIOLATION: <1-line summary>
Sizing:   ok | <file>:<old>→<new> (over soft/hard cap)
Modules:  ok | <module>: <issue>
Isolation: ok | <1-line summary of unexpected files>
Verify:   cargo fmt/clippy/test — pass | fail: <summary>

Recommendations:
- <bullet per concrete action, file:line where possible>
```

Keep it under ~25 lines. If everything is ok, say so in one line and stop.

## 5. What this skill does NOT do

- It does not run `cargo fix` or auto-refactor. Refactors are judgment calls; surface the problem and let the user or the main agent decide the cut.
- It does not update `docs/architecture.md`. That is handled by the doc-sync subagent invoked at spec completion (see `CLAUDE.md`).
- It does not enforce style beyond what `cargo fmt` and `cargo clippy` already enforce. Style is not the concern here — structure is.
- It does not block on the existing oversize files (`context.rs`, `tui.rs`, `session_events.rs`). It only blocks on **growing** them further or introducing **new** oversize files.
