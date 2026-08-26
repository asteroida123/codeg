# ADR 0001: Arena — clean-room rebuild on the existing execution core

- **Status:** Accepted
- **Date:** 2026-08-26
- **Scope:** multi-agent Arena feature (formerly PR #534), Batch composition, workbench contribution boundaries

## Context

PR #534 ("Agent Arena": compare several agents on one task in real time) was
merged, reverted, and its traces force-pushed out of `main`. Two independent
reviews (Claude Code + Codex) both returned **REWORK** on the artifact as
submitted. The product direction — same task, same base, different
agents/models/configs, side-by-side — was not disputed; the execution shape
was. Summarized findings:

1. An unborn-HEAD repo got its user's **staged index committed** into a seed
   root commit by a generic `git_worktree_add` path, with a hardcoded author.
2. Worktree **cleanup could never succeed** for a linked worktree when the
   worktree itself was passed as `repo_path` (the `canonical == canonical_repo`
   guard fires), yet the UI reported success and left every worktree/branch on
   disk permanently.
3. The contest workspace was created **inside the user's repository**
   (`.codeg-pk/`), permanently polluting `git status` and risking an embedded
   gitlink on `git add -A`.
4. The shareable report interpolated one dynamic field **unescaped** (XSS),
   ran scripts by default in a sandbox **weaker than the app's existing
   `HtmlPreview`**, and allowed escaping to a top-level `blob:` window.
5. The LLM judge ran in the user's real repo with the contestants' raw diff as
   prompt, and its permission requests were never answered (infinite hang).
6. Cancel had no generation/abort gate: worktrees kept being created and
   contestants were written back to `ready` after cancellation.
7. The 1421-line front-end orchestrator was the **authoritative state machine**
   (window close, second window, second server client changed execution
   semantics) and deliberately bypassed the delegation broker.
8. 48 tests for ~13.7k lines; the orchestrator's lifecycle and error paths had
   effectively no coverage.

The architectural review that followed (see
[`docs/architecture-review-gate.md`](../architecture-review-gate.md))
concluded the problems are not fixed by tuning the old branch, and the code
must not be resurrected as-is.

## Decision

Codeg remains a multi-agent coding workspace. The existing **WorkTask Engine**
is the single execution authority. The Arena becomes a **first-party Workbench
App** built on generic seams — no Arena entity ever enters Core.

1. **Freeze, do not resurrect.** The `feat/agent-pk-arena` branch is frozen as
   a product/UX specification and a catalog of counter-examples. Its
   execution, worktree, judge, and report code is not ported; product behavior
   is re-implemented clean-room.
2. **One execution authority.** Long tasks, worktrees, cancellation, permission
   blocking, review, and cleanup happen only in the backend WorkTask Engine
   (`src-tauri/src/work_task/engine.rs`). Front-ends send commands and
   subscribe to events; they never manage connections or own state machines.
3. **Generic seams, second consumer required.** New Core primitives are only
   added when existing capabilities cannot cover the gap and at least two
   consumers exist. The first candidates are:
   - `ResolvedLaunchProfile` — requested vs. applied capability snapshot
     (model/effort/permission/skills/mcp), per-session isolation.
   - `WorkTaskBatch` — shared immutable `base_sha`, members as existing
     WorkTasks, aggregate start/cancel/cleanup, per-member results.
   - Workbench **Contribution Registry** — route/navigation/view registration,
     migrated from the hardcoded route union.
   - Workflow Coordinator — phase/gate/dependency only; steps reference
     WorkTask/Batch.
4. **Core knows no business nouns.** `pk_round`, `contestant`, `judge`,
   `arena`, `ccg_strategy` have no home in Core entities, services, events,
   or route switches.
5. **Default-safe everywhere.** The report model follows the existing
   `HtmlPreview` model: fully escaped output, scripts/network disabled by
   default, explicit per-file trust to enable a restricted sandbox + CSP, no
   top-level execution of agent output. A static, escaped single-file report
   ships first; runnable preview and free-text LLM judge are deferred and
   require a separate security review.
6. **Contributions land in small PRs.** Backend primitives, app UI, and
   marketing/share surfaces never share a PR (see the review gate's PR-split
   rules).

## Blocker regression coverage

Each of the findings above is pinned as a regression test on `main`'s generic
layer so the invariants survive regardless of how the Arena is later built.
The mapping below is the "blocker backlog" of the rework.

| Finding | Generic invariant | Regression test |
|---|---|---|
| 1. unborn-HEAD seed commit | `git_worktree_add` never commits; unborn HEAD fails cleanly with the index untouched; an explicit base works without consuming staged changes | `worktree_add_on_unborn_head_refuses_without_seeding_a_commit`, `worktree_add_with_explicit_base_leaves_user_index_untouched` (folders.rs tests) |
| 2. cleanup swallowing | Worktree removal returns explicit errors; using the worktree itself as `repo_path` is refused by the `canonical == canonical_repo` guard and leaves tree + branch intact; callers must surface per-member results | `remove_worktree_with_the_worktree_itself_as_repo_path_refuses_and_leaves_it_intact` (folders.rs tests) |
| 3. repo pollution | Contest/derived state never creates directories inside the user's repository; ignore entries only cover Codeg's own concerns | ADR §Decision 5 + review gate hygiene rules (`.zcode`, workspace-local dirs stay out of the repo's `.gitignore`) |
| 4. report XSS / sandbox | All dynamic fields escaped; previews default to `sandbox=""` (no scripts); trust is explicit per file and limited (`allow-scripts allow-popups allow-forms allow-modals`, no `allow-same-origin`, no `allow-top-navigation`); CSP injected via `withSandboxCsp` | `html-preview.test.tsx` (this PR); `html-preview-inline.test.ts` (existing) |
| 5. judge in real repo / hang | Judge/review-style tasks are future app code; Core already exposes read-only and timeout primitives — the App must run evaluation without write access and with deadlines | covered by ADR §Decision 6; enforced at App review time |
| 6. cancel without gate | Cancellation settles via `run_seq`/CAS; late events for an old generation are no-ops | `a_late_cancelled_event_does_not_cancel_a_task_in_review`, `stale_recovery_cannot_bounce_a_newer_generation`, `a_stale_head_on_this_base_bounces_instead_of_duplicating` (engine.rs, existing) |
| 7. front-end authority | Execution semantics survive window close/refresh/multi-client because the backend owns state | enforced by the review gate (I1/I3) on every future PR; work_task engine tests cover restart/recovery paths |
| 8. lifecycle coverage | New orchestration ships with its lifecycle, cancel, cleanup, and fault tests (not only pure functions) | enforced by the review gate (test pyramid) on the rework PRs |

## Consequences

- The frozen branch stays local-only; it is no longer pushed to any public
  remote and never becomes a merge base.
- Arena V1 scope is: 2–4 slots, same agent with different `ResolvedLaunchProfile`
  under real capability isolation, shared base SHA via `WorkTaskBatch`,
  live transcript/diff/preflight/metrics, batch cancel with per-member cleanup
  results, and a static escaped report. No LLM judge, no runnable embedded
  preview, no elimination tournaments.
- New architecture decisions (e.g., Workflow Coordinator for CCG, Workbench
  Contribution Registry) will extend this ADR series rather than reopen it.

## Explored and rejected (2026-08-27) — task/session skill controls

Evaluated three ways to give tasks/sessions control over which skills an agent
sees, and rejected all of them for now:

1. **Session-level Skill Policy (**#541**, inherit/none/selected).** Agent
   support is uneven (native flags/envs differ per agent); a first version
   would serve 1–2 agents only, which is a narrow slice of users for a new
   per-connection mechanism.
2. **Per-connection config-root redirection** (e.g. `CLAUDE_CONFIG_DIR` /
   `CODEX_HOME` pointing at a session-owned copy, empty/whitelist `skills`
   subdir) to make `none` truly hide global skills. Works, but it is a
   per-agent adapter project whose only purpose is "hide global skills".
   Global-skill noise can already be controlled by the per-agent matrix, so the
   added value did not justify the adapter surface.
3. **Worktree-level assembly** (symlink selected skills into `wt/.claude/skills`
   etc., agent discovers by cwd convention — uniform across 14/15 agents).
   Cheap and uniform, but it can only **add** skills, never **hide** them
   (global matrix skills remain visible in every task). Without the "hide"
   half, the mechanism's edge over a prompt instruction ("don't use X") is
   limited to context savings and verifiability, and its use case — tasks
   needing skills outside the global matrix — is narrow.

Revisit only when one of these holds: a uniform mechanism exists to make agent
processes unable to discover skills (not per-agent adapters), or a concrete
consumer (e.g. `WorkTaskBatch` configuration experiments comparing
with/without skills) is being built. The `SkillPolicy` value type lives on in
`launch_profile.rs` as part of the launch-profile snapshot; execution stays
out.

## `WorkTaskBatch` — the composition primitive (2026-08-26)

The second generic seam of Decision 3, and the one Arena's execution depends
on. Shape and the reasoning behind each choice:

**A join table, not a column.** Membership lives in `work_task_batch_member`
rather than a `batch_id` on `work_task`. The task table and the ~10.5k-line
engine that owns it are untouched, and the columns a batch adds — slot order,
label, launch snapshot, cleanup outcome — live where they belong instead of
widening every task row in the database. The unique index on `task_id` keeps
the engine's lookup unambiguous: a task cannot be in two batches pinning
different commits.

**The batch status is a projection, not a state machine.** `status` is derived
from the members by `recompute_status` and stored so a client reads one row
instead of N, and so "this round finished" survives a restart. It grants no
authority: the batch service never writes a `work_task` status, and every
member transition still goes through the engine's CAS-guarded paths. The
`review` value is deliberate — a round whose agents have all stopped but whose
results nobody has accepted is neither "never started" nor "finished".

**Cancellation is one-way, and guarded on the write.** `batch_cancel` records
the batch's cancellation *before* touching any member, and the projection's
UPDATE carries `WHERE status != 'canceled'`. Both halves are needed: the
read-side check cannot see a cancel that lands after it, and without the write
condition a projection computed a moment earlier lands last and erases the
user's decision — the batch would report a normal completion after being
canceled, which is finding 6 one level up. A canceled batch never reopens;
retrying a member is allowed and runs, but a fresh round means a fresh batch,
resolved against a fresh base.

**One base commit, pinned once, verified at use.** `resolve_base` reads the
project folder's HEAD at creation and refuses three states outright: not a
repository, detached HEAD, no commits yet, plus modified tracked files unless
the caller passes `allow_dirty`. Nothing on this path writes to the
repository — no `add`, no `commit`, no `stash` — which is finding 1 answered at
the source. `batch_checkout_point` then applies that commit in place of HEAD
when the engine builds a member's worktree, mirroring the existing
`pr_checkout_point` seam, and re-verifies the object still exists so a
rewritten history produces a sentence about the batch rather than
`fatal: not a valid object name`. Errors here are never downgraded to a
fallback: a member launched on a base its siblings do not share would make a
quietly meaningless comparison, which is worse than a visible failure.

**Aggregates answer per member.** `start`, `cancel`, and `cleanup` each return
one outcome per member, and cleanup persists each outcome on its member row
*before* returning it — including the failures, and including the case where
the ledger write itself fails (reported `failed`, because a success no later
screen can corroborate is the exact false success of finding 2). `blocked` is
distinct from `failed` on purpose: one says stop the member first, the other
offers a retry.

**Core stays free of business nouns.** `owner_extension` carries a reverse-DNS
id and `metadata` an opaque JSON blob; neither is ever branched on. A batch has
members with slots and labels — what a slot *means* is the caller's business.
Grepping the new Core files for the forbidden vocabulary returns nothing,
comments included.

Coverage: 35 tests — 18 on the service (projection walk, the cancel gate on
both read and write, atomic create, deleted members, the cleanup ledger), 6 on
the git base resolution (unborn HEAD leaves the staged index intact, dirty tree
refused and never tidied, detached HEAD, exact-commit pinning, untracked files
allowed), and 11 driving the engine's aggregates against a real database
(cancel-before-touch ordering, per-member start answers, no re-claim of working
members, retry of failed ones, `blocked` persistence, keep-list, `fail_fast`
sparing in-flight work, `best_effort` continuing).