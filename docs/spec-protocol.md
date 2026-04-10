# Spec Protocol

This project is built semi-autonomously from scoped specs in `specs/`. This document is the protocol Claude follows when authoring or executing one.

## Why specs exist

Specs are the contract between the human and Claude for a unit of work. They constrain scope, name the acceptance criteria up front, and pre-declare the points where Claude must stop and check in. The goal is to let Claude run free *within* a well-defined box, not to micromanage every step.

## Authoring a spec

When the user describes work in rough terms ("let's add /clear and /compact slash commands"), Claude's job is to expand that into a spec file before writing any code.

1. **Copy the template.** Start from `specs/_template.md`. Save as `specs/NNNN-short-name.md` where `NNNN` is the next four-digit number in `specs/` (zero-padded, e.g. `0001`, `0042`, `1337`). Use a kebab-case slug.
2. **Fill in every section.** Don't leave placeholders. If a section doesn't apply, write "none" — empty sections are a smell that the spec isn't ready.
3. **Read before writing.** Before filling in *Context* and *Acceptance criteria*, actually read the files you're going to list. A spec built from guesses produces broken plans later.
4. **Make acceptance criteria checkable.** "Works correctly" is not a criterion. "Test `foo::bar::test_handles_empty_input` passes" is.
5. **Out of scope is mandatory.** Every spec has tempting adjacent work. Name it explicitly so future-Claude doesn't quietly expand the box.
6. **Set status to `draft`.** Then stop and ask the user to review. Do not begin execution from a draft spec.
7. **On approval**, flip status to `ready`. If the user approves authoring and execution in the same message ("looks good, run it"), skip `ready` and go straight to `in-progress` — the explicit-gate step exists to prevent execution without approval, not to add ceremony when approval is already in hand.

## Executing a spec

When the user points Claude at a `ready` spec, follow this loop.

1. **Read the spec in full**, then read every file listed in *Context*. If something is unclear or a context file no longer exists, stop and report — do not guess.
2. **Flip status** from `ready` to `in-progress`.
3. **Post a plan.** 5 lines or fewer: what you'll change, in what order, and which acceptance criterion each step satisfies. Then **stop at the first checkpoint** (every spec has "post plan, wait for go/no-go" as its first checkpoint by default).
4. **On go-ahead, run free** until the next checkpoint or until all acceptance criteria are met. Run the verify loop after each meaningful change, not just at the end.
5. **Never expand scope.** If you discover something in *Out of scope* is actually required, stop and report — propose a spec amendment, don't silently do the work.
6. **At each checkpoint**, stop and post: what's done, what's next, any deviations from the plan. Wait for go/no-go.
7. **If blocked**, append findings to the *Notes* section of the spec, set status back to `ready` (or leave as `in-progress` with a clear blocker note), and stop. Do not thrash.
8. **Use Notes for decisions, not just blockers.** Whenever you make a non-obvious judgment call during execution (parser shape, error message wording, whether to migrate adjacent code), append it to *Notes* with a one-line reason. Future readers — including future-Claude on a related spec — should be able to reconstruct *why* the code looks the way it does without re-deriving it.
9. **On completion**, run the full verify loop one final time.
10. **Run the judge pass** (see below). If the judge is clean, flip status to `done` and propose a commit. If the judge flags anything, do **not** flip status — post the verdict, wait for go/no-go. Do not commit without explicit approval.

## Judge pass

Every spec gets an independent cold-reader review before it can move to `done`. The judge exists because the executing Claude marks its own homework — it knows what it built, what tradeoffs it made, and will rationalize "this basically meets criterion #3" when a fresh reader wouldn't.

**Why every run, not "when triggered":**
- Self-bias is always present, not just on complex specs. The specs Claude is most confident about are the specs most likely to have skipped corners.
- Running every time builds calibration — you learn what a clean verdict looks like, so deviations stand out. Trigger-based runs make every verdict a deviation, so nothing is comparable.
- A trigger-evaluation step ("should I run the judge?") is itself decision overhead. Always-run removes a decision point.
- The cost is small: one subagent call, seconds and a few thousand tokens.

**How to invoke.** Use the Agent tool with the `general-purpose` subagent. Pass the verbatim prompt below, with two substitutions:

- `{SPEC_PATH}` — absolute or repo-relative path to the spec file
- `{DIFF_COMMAND}` — the shell command that produces the diff to review. Choose based on lifecycle stage:
  - Uncommitted work: `git diff HEAD`
  - In-flight branch: `git diff main...HEAD`
  - Post-commit review: `git diff HEAD~1`

The judge gets **no execution context**: no plan, no notes, no Claude-side rationale, no "here's why I made this decision." The whole point is independence — briefing the judge on your reasoning poisons the cold read. Do not deviate from the prompt below; consistent wording across runs is what makes verdicts comparable.

### The judge prompt (verbatim)

```
You are an independent code reviewer performing a cold read. You have NO
context about how this work was done — only the spec and the diff. Do not
assume the executor was right. Do not give the benefit of the doubt. Borderline
cases are AMBIGUOUS, not MET.

Inputs:
- Spec file: {SPEC_PATH}
- Diff command: {DIFF_COMMAND}

Procedure:
1. Read the spec file in full. Pay attention to "Acceptance criteria" and
   "Out of scope".
2. Run the diff command and read the full diff.
3. For each acceptance criterion, also read the relevant code in its
   post-diff state. A criterion may depend on code the diff did not touch —
   do not trust the diff alone.
4. Answer the three questions below in the exact format specified. No
   preamble, no summary, no commentary outside the format. Start directly
   with "## Question 1".

## Question 1: Per-criterion verdict

For EACH item in the spec's "Acceptance criteria" section, give one verdict:

- MET — provably satisfied. Cite the file:line or test name that proves it.
- NOT MET — not satisfied. Explain what is missing.
- AMBIGUOUS — the criterion text itself is unclear or unprovable as written.
  Explain what is unclear. Do NOT guess. If you cannot tell what would count
  as satisfying it, mark AMBIGUOUS — do not infer intent.

Format (one bullet per criterion, verbatim criterion text):
- [criterion text]: MET — [evidence: file:line or test name]
- [criterion text]: NOT MET — [what is missing]
- [criterion text]: AMBIGUOUS — [what is unclear]

## Question 2: Out-of-scope check

Does the diff include any change that falls under the spec's "Out of scope"
section? For each violation, name the file:line and the specific Out-of-scope
item it violates.

Format:
- VIOLATION: [file:line] — violates "[out-of-scope item]" — [what the diff does]

Or, if none:
- None found.

## Question 3: Scope-creep check

Does the diff include any change that no acceptance criterion asked for?
This catches "while I was in there" additions: tests for behaviors not in
the AC, refactors of adjacent code, helpful-but-unrequested error handling,
extra documentation, new helpers, renamed symbols.

For each finding, name the file:line and the change. Do NOT decide whether
it should stay or go — surface it and note that no AC covers it.

Format:
- CREEP: [file:line] — [what the change is] — no AC covers this

Or, if none:
- None found.

## Overall verdict

One line:
- CLEAN — if every criterion is MET, no Out-of-scope violations, no creep.
- NEEDS ATTENTION — otherwise.

Constraints:
- Be strict, not generous. Borderline = AMBIGUOUS.
- Be specific. "Looks good" is not evidence; "tui.rs:823" is.
- Do not propose fixes. You are evaluating, not advising.
- Do not write a preamble or summary. Start with "## Question 1".
```

**Handling the verdict:**

- **All `met`, no out-of-scope, no scope creep** → judge is clean. Flip status to `done`, propose commit.
- **Any `not met`** → fix the code, re-run verify, re-run judge. Do not amend the criterion to make it pass — that's defeating the purpose.
- **Any `ambiguous`** → the criterion was bad. Surface to the user, propose either a code change or a spec amendment. Do not silently decide which way it goes.
- **Out-of-scope violation** → either remove the change from the diff or surface to the user with a proposed *Out of scope* amendment. Never silently keep it.
- **Scope creep** → surface each finding. Some will be worth keeping (and the AC should grow to cover them); some should be removed. The user decides.

**The escalation rule:** if the judge disagrees with Claude's self-assessment, Claude does **not** override the judge silently. The judge has veto-as-pause-button — every disagreement surfaces to the user. Override is a human decision, not a Claude decision.

**Anti-patterns for the judge:**

- **Briefing the judge.** "Here's what I built and why" defeats independence. Spec + diff + questions, nothing else.
- **Re-running until it agrees.** If the judge flags something twice, the answer is to fix the code or amend the spec, not to coax the judge into a clean verdict.
- **Treating the judge as the final reviewer.** It isn't. The human is. The judge is a structured pre-check that catches things before they reach the human.

## Retros

A retro is a deliberate reflection on *how a spec went*, separate from the work itself. They are valuable but easy to over-do — most specs don't need one.

**When to retro:**
- A checkpoint was in the wrong place (too early, too late, or missing entirely)
- Scope drifted, or *Out of scope* turned out to be wrong
- The verify loop missed something the user caught
- Acceptance criteria turned out to be unprovable, ambiguous, or incomplete
- Something about the protocol itself got in the way, or a protocol rule produced an unexpectedly good outcome worth reinforcing
- The user explicitly asks for one

**When not to retro:**
- Spec executed cleanly with no surprises — the lack of friction is the signal
- Routine work where every decision was obvious from context
- "Just to be thorough" — that's noise, not insight

**Format:** a short bulleted section appended at the end of the spec's *Notes*, headed `### Retro`. Each bullet is one of: *what surprised me*, *what I'd do differently*, *what worked and should be repeated*. Two to five bullets is typical; if you have more, you're probably writing filler.

**Escalation:** if a retro bullet is about *the protocol or the project*, not just *this spec*, it should escalate beyond Notes — propose an edit to `docs/spec-protocol.md`, or save it as a feedback memory. A retro insight that only lives in a single spec's Notes will be forgotten.

## What stays out of specs

- Code patterns and conventions — those live in `CLAUDE.md`.
- The verify loop command — also in `CLAUDE.md`. Specs reference it, don't redefine it.
- Long design rationale — that belongs in `docs/`. Specs link to it via *Context*.

## Anti-patterns

- **Vague acceptance criteria** ("the TUI feels responsive"). Reject and tighten.
- **Drafts that skip context-reading.** A spec authored without reading the code is fiction.
- **Checkpoint creep.** If every step is a checkpoint, the spec isn't semi-autonomous — it's manual work with extra ceremony. Default to one checkpoint (after the plan) unless the task has a genuine architectural seam.
- **Silent scope expansion.** The single biggest failure mode. *Out of scope* exists to be enforced.
