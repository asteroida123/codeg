# Codeg Architecture Review Gate

**Status:** accepted baseline for major-change review (2026-08-26)

Purpose: prevent a repeat of "the product value is real, but the implementation
bypassed the core, lost state authority, left security debt, and could not be
reviewed." Applies to multi-agent Apps (Arena, Eval, Team…), Workflows (CCG,
PR Review…), Workbench pages, dynamic UI, plugins, task-engine and data-model
changes.

A one-page checklist any design/PR must pass before code is written, and again
before merge. Any **hard stop** below ⇒ REWORK regardless of score.

## The 12-question gate

| # | Question | Passing evidence |
|---|---|---|
| 1 | Is the problem real? | User scenario, reproduction, or measurable gain — not a feature derived from existing capability |
| 2 | Is the classification right? | Core / generic runtime / Workflow / Workbench App / Skill / MCP / dynamic UI is decided |
| 3 | Are existing engines exhausted? | Reuse verdict for Conversation, ACP, Delegation, WorkTask, Git/worktree, MCP, Skills is documented |
| 4 | Does Core contain business nouns? | Core only has generic concepts (batch, profile, contribution, gate); `pk_round`, `judge`, `ccg_strategy` never appear |
| 5 | Where is the execution truth? | Long tasks, cancel, retry, cleanup, recovery live in a backend authoritative state machine |
| 6 | Does closing a window change semantics? | Desktop, Server, second client, and reconnect behave identically |
| 7 | Is the user's repository safe? | No staged-index commits, no directory pollution, no swallowed cleanup failures, no fake success |
| 8 | Can untrusted content execute? | Escaping, no scripts by default, CSP, explicit trust, restricted permissions |
| 9 | Is the failure model written? | Cancel races, permission waits, crash recovery, disk residue, partial success all have states |
| 10 | Do tests cover the lifecycle? | Integration, concurrency, fault injection, security regression — not just pure functions |
| 11 | Can the PR be reviewed? | Generic primitives, App, marketing/share, and security features are separate PRs |
| 12 | Is the public repo clean? | No internal strategy, competitor wording, unreferenced large assets, unrelated ignore entries |

## Four invariants that must never break

- **I1 — Single execution authority.** Engineering execution goes through the
  existing backend task/connection capabilities. Apps and Workflows do not
  build parallel state machines. *(Who owns start, cancel, permission, recovery,
  cleanup?)*
- **I2 — Core has no business nouns.** A new Core API must remain independently
  valuable with the App deleted.
- **I3 — Persistent facts live in the backend.** State that affects execution
  or recovery is persisted, replayable, and observable by multiple clients.
  *(Refresh, restart, second window: same truth.)*
- **I4 — Default-safe, truthful feedback.** Untrusted content does not execute
  by default; failures are visible; the UI never reports success after a
  backend failure. *(Check `Promise.allSettled` results before claiming
  success.)*

## Hard stops (any one ⇒ REWORK)

1. Bypassing the existing execution engine (copying the Agent/Task/Worktree
   state machine without approval).
2. Front-end as authoritative state machine (refresh/close/second client
   changes execution semantics).
3. User-data risk (committing staged content, polluting the repo, deleting
   branches/directories without recovery).
4. Untrusted code executes by default (HTML/plugin/artifact without explicit
   trust).
5. Failures swallowed (UI shows success or clears recovery info after backend
   failure).
6. No lifecycle tests (execution, cancel, cleanup, crash paths lack
   integration coverage).
7. Unrecoverable changes (migration/resource changes cannot be rolled back).
8. Public-repo leakage (internal strategy, sensitive material, unrelated large
   files on main).

## Classification decision tree (condensed)

```
Does it change real execution, persisted facts, or security boundaries?
├─ No → behavior guidance only?        → Skill
│     external tool capability?        → MCP
│     one-off/temporary UI?            → Dynamic UI / A2UI
└─ Yes → reusable across scenarios and backend-enforced? → Core / generic runtime
         stage/dependency/gate/policy only?              → Workflow package
         stable complete product experience?              → Workbench App
Plugin is packaging/lifecycle/distribution, not new execution semantics.
```

## Naming audit

| Place | Acceptable | Requires rework |
|---|---|---|
| Core entity/migration | `work_task_batch`, `launch_profile`, `contribution`, `artifact_snapshot` | `pk_round`, `arena_player`, `judge_result`, `ccg_phase` |
| Core service | `TaskBatchService`, `LaunchProfileResolver`, `ContributionRegistry` | `ArenaService`, `CcgEngine` |
| App storage | `arena.round`, `arena.judge` (namespaced) | — (business nouns belong here) |
| Events | `task.batch.started`, `workflow.gate.blocked` | `pk.contestant.running` (unless App-private) |

## Default review budget

- Single Core PR: ~800–1,500 hand-written lines, at most 2–3 core subsystems.
- >20 changed files or >2,000 hand-written lines: must attach a split rationale
  and maintainer waiver.
- Migration + runtime state machine + complex UI + 10-language i18n + export
  never first appear together.
- Generated/translated/doc noise is counted separately and must not hide the
  real diff.

## Default PR split

| PR | Contents | Must not mix in |
|---|---|---|
| 0 | Known-bug regression fixes + tests (staged index, cleanup, XSS, cancel race) | New product UI |
| 1 | Generic backend primitives (LaunchProfile, WorkTaskBatch, generic events/API) | Arena/CCG business nouns |
| 2 | Contribution/UI seams (route, navigation, chrome, settings registration) | Full Arena |
| 3 | First consumer (migrate an existing page, or a minimal Arena vertical slice) | Marketing reports, runnable preview |
| 4 | Product App/Workflow stable experience | New Core lifecycle |
| 5 | Share, judge, dynamic UI — with its own security review and release budget | "While we're at it" additions |

## Review process

1. **Two-phase.** Design review (problem, classification, reuse, state
   authority, security boundary, split plan) before code; implementation review
   (code, tests, performance, cross-platform) before merge.
2. **Independent reviewers.** Two reviewers read the same RFC/diff
   independently, one on product/boundaries/reuse, one on code/security/
   concurrency. Every blocker cites path + line + call chain or a reproducible
   command. Each reviewer actively corrects at least one of their own initial
   judgments.
3. **Evidence over opinion.** "Feels risky" is not a blocker; "canonical ==
   canonical_repo fires because repo_path is the worktree itself" is.
4. **Maintainer decides.** Two models agreeing does not auto-pass or
   auto-fail; the maintainer weighs evidence, roadmap, and release window.

## Repository hygiene for public contributions

- Internal decision memos, monetization analysis, competitor remarks, and
  "worth copying" phrasing stay out of public branches.
- Process records, scratch roadmaps, and unreferenced images live on
  external branches / Discussion / Artifacts — not in product code.
- Ignore entries only cover Codeg's own concerns; personal tooling (`.zcode/`)
  and app-local directories (`.codeg-pk/`) must not be added to the repo's
  `.gitignore`.
- PR descriptions distinguish "tests run" from "maintainer integration
  verified"; green CI is not a merge license.
- Large product features open an Issue/ADR first to lock the boundary before
  implementation.

## References

- ADR 0001: [Arena — clean-room rebuild on the existing execution core](./adr/0001-arena-clean-room-reuse-foundation.md)
- Full review norm (v1.0, with scoring table and templates) is maintained as
  an internal artifact; the gate above is the public, enforceable subset.