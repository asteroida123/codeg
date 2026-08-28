import { describe, expect, it } from "vitest"

import type {
  BatchCleanupOutcome,
  WorkTaskBatch,
  WorkTaskBatchMember,
  WorkTaskStatus,
} from "@/lib/types"
import {
  memberLabel,
  memberPhase,
  refusedStarts,
  batchControls,
  batchDiffTotals,
  summarizeCleanup,
} from "./work-task-batch-model"

function member(over: Partial<WorkTaskBatchMember> = {}): WorkTaskBatchMember {
  return {
    id: 1,
    batch_id: 1,
    task_id: 11,
    slot_index: 0,
    task_status: "todo",
    task_title: "slot",
    conversation_id: null,
    connection_id: null,
    files_changed: null,
    additions: null,
    deletions: null,
    ...over,
  }
}

function batch(members: WorkTaskBatchMember[]): WorkTaskBatch {
  return {
    id: 1,
    folder_id: 7,
    title: "compare",
    base_sha: "a".repeat(40),
    base_branch: "main",
    status: "created",
    failure_policy: "best_effort",
    members,
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    settled_at: null,
  }
}

describe("memberPhase", () => {
  it("groups every task status into exactly one phase", () => {
    const cases: [WorkTaskStatus, string][] = [
      ["todo", "waiting"],
      ["queued", "waiting"],
      ["preparing", "working"],
      ["running", "working"],
      ["merging", "working"],
      ["awaiting_input", "blocked"],
      ["review", "finished"],
      ["done", "finished"],
      ["failed", "stopped"],
      ["canceled", "stopped"],
    ]
    for (const [status, phase] of cases) {
      expect(memberPhase(status), status).toBe(phase)
    }
  })

  /** A member whose task row was deleted will never report again, so offering it
   *  a start would be a lie. */
  it("treats a missing task row as stopped, not waiting", () => {
    expect(memberPhase(null)).toBe("stopped")
  })
})

describe("batchControls", () => {
  it("offers a start while any member has never run", () => {
    const c = batchControls(batch([member({ task_status: "todo" })]))
    expect(c.canStart).toBe(true)
  })

  it("offers a start for a failed member, so a casualty can be retried", () => {
    const c = batchControls(batch([member({ task_status: "failed" })]))
    expect(c.canStart).toBe(true)
  })

  it("offers no start once every member has finished", () => {
    const c = batchControls(
      batch([
        member({ task_id: 11, task_status: "review" }),
        member({ task_id: 12, task_status: "done" }),
      ])
    )
    expect(c.canStart).toBe(false)
  })

  /** Cancellation is one-way; a canceled round is not a launch pad. A user who
   *  wants another go starts a new round, which also re-resolves the base. */
  it("offers no start on a canceled round", () => {
    const r = batch([member({ task_status: "canceled" })])
    r.status = "canceled"
    expect(batchControls(r).canStart).toBe(false)
  })

  it("offers a cancel while anything is live or queued", () => {
    expect(
      batchControls(batch([member({ task_status: "running" })])).canCancel
    ).toBe(true)
    expect(
      batchControls(batch([member({ task_status: "awaiting_input" })]))
        .canCancel
    ).toBe(true)
    expect(
      batchControls(batch([member({ task_status: "queued" })])).canCancel
    ).toBe(true)
    expect(
      batchControls(batch([member({ task_status: "done" })])).canCancel
    ).toBe(false)
  })

  /** The backend refuses a live member's cleanup and reports it `blocked`; not
   *  offering the button avoids inviting an action that is guaranteed to be
   *  partly refused. */
  it("withholds cleanup while a member is still live", () => {
    const c = batchControls(
      batch([
        member({ task_id: 11, task_status: "running" }),
        member({ task_id: 12, task_status: "review" }),
      ])
    )
    expect(c.canCleanup).toBe(false)
  })

  it("offers cleanup once nothing is live and something is left to remove", () => {
    const c = batchControls(
      batch([
        member({ task_id: 11, task_status: "review" }),
        member({ task_id: 12, task_status: "failed" }),
      ])
    )
    expect(c.canCleanup).toBe(true)
    expect(c.cleanableCount).toBe(2)
  })

  it("stops offering cleanup for members already cleaned", () => {
    const c = batchControls(
      batch([
        member({
          task_id: 11,
          task_status: "done",
          cleanup_result: "succeeded",
        }),
        member({
          task_id: 12,
          task_status: "done",
          cleanup_result: "succeeded",
        }),
      ])
    )
    expect(c.canCleanup).toBe(false)
    expect(c.cleanableCount).toBe(0)
  })

  /** A previous failure has to stay actionable — that is the whole reason the
   *  outcome is persisted on the member row. */
  it("keeps offering cleanup to a member whose last attempt failed", () => {
    const c = batchControls(
      batch([
        member({
          task_id: 11,
          task_status: "done",
          cleanup_result: "failed",
          cleanup_error: "worktree is locked",
        }),
      ])
    )
    expect(c.canCleanup).toBe(true)
    expect(c.cleanableCount).toBe(1)
  })

  /** Dismissal is the exit for a finished group — without it, the strip keeps
   *  the row forever, because nothing else ever removes it. Offered only once
   *  the batch is over AND fully cleaned: the soft delete drops the grouping
   *  row alone, so removing the one row that still explains a worktree or a
   *  pending decision must not be invited. */
  it("offers dismissal only for an over batch with nothing left to clean", () => {
    const over = batch([
      member({ task_id: 11, task_status: "done", cleanup_result: "succeeded" }),
    ])
    over.status = "settled"
    expect(batchControls(over).canDismiss).toBe(true)
    const canceled = batch([
      member({
        task_id: 11,
        task_status: "canceled",
        cleanup_result: "succeeded",
      }),
    ])
    canceled.status = "canceled"
    expect(batchControls(canceled).canDismiss).toBe(true)
  })

  it("withholds dismissal while anything is cleanable or still pending", () => {
    const stillCleanable = batch([
      member({ task_id: 11, task_status: "done", cleanup_result: "succeeded" }),
      member({ task_id: 12, task_status: "done" }),
    ])
    stillCleanable.status = "settled"
    expect(batchControls(stillCleanable).canDismiss).toBe(false)

    const inReview = batch([
      member({
        task_id: 11,
        task_status: "review",
        cleanup_result: "succeeded",
      }),
    ])
    inReview.status = "review"
    expect(batchControls(inReview).canDismiss).toBe(false)

    const running = batch([member({ task_id: 11, task_status: "running" })])
    running.status = "running"
    expect(batchControls(running).canDismiss).toBe(false)
  })
})

describe("batchDiffTotals", () => {
  it("sums the members, counting an absent measurement as nothing", () => {
    const totals = batchDiffTotals(
      batch([
        member({ task_id: 11, files_changed: 3, additions: 40, deletions: 5 }),
        member({
          task_id: 12,
          files_changed: null,
          additions: null,
          deletions: null,
        }),
        member({ task_id: 13, files_changed: 1, additions: 2, deletions: 0 }),
      ])
    )
    expect(totals).toEqual({ filesChanged: 4, additions: 42, deletions: 5 })
  })
})

describe("memberLabel", () => {
  it("prefers the label, falling back to the task title", () => {
    expect(memberLabel(member({ label: "Codex high" }))).toBe("Codex high")
    expect(memberLabel(member({ label: null, task_title: "fix login" }))).toBe(
      "fix login"
    )
    // A label of only spaces is not a name.
    expect(memberLabel(member({ label: "   ", task_title: "fix login" }))).toBe(
      "fix login"
    )
  })
})

/**
 * The false-success guard, at the layer that decides what the user is told.
 *
 * The implementation this feature replaces reported one cleanup success while
 * every worktree stayed on disk. These tests pin that `allSucceeded` is true in
 * exactly one situation, and that the members that did not succeed survive with
 * their reasons.
 */
describe("summarizeCleanup", () => {
  const outcome = (
    task_id: number,
    result: BatchCleanupOutcome["result"],
    error?: string
  ): BatchCleanupOutcome => ({ task_id, slot_index: 0, result, error })

  it("reports success only when every attempted member succeeded", () => {
    const all = summarizeCleanup([
      outcome(11, "succeeded"),
      outcome(12, "succeeded"),
    ])
    expect(all.allSucceeded).toBe(true)
    expect(all.unresolved).toHaveLength(0)
  })

  it("does not report success when a member failed", () => {
    const s = summarizeCleanup([
      outcome(11, "succeeded"),
      outcome(12, "failed", "worktree is locked by another process"),
    ])
    expect(s.allSucceeded).toBe(false)
    expect(s.succeeded).toBe(1)
    expect(s.failed).toBe(1)
    expect(s.unresolved).toHaveLength(1)
    expect(s.unresolved[0].error).toBe("worktree is locked by another process")
  })

  /** `blocked` is not a failure, but it is not a success either — the worktree is
   *  still there. Counting it as done is how a UI ends up lying. */
  it("does not report success when a member was blocked", () => {
    const s = summarizeCleanup([
      outcome(11, "succeeded"),
      outcome(12, "blocked"),
    ])
    expect(s.allSucceeded).toBe(false)
    expect(s.blocked).toBe(1)
    expect(s.unresolved.map((o) => o.task_id)).toEqual([12])
  })

  /** Nothing attempted is not success: claiming it would describe work that
   *  never happened. */
  it("does not report success for an empty result set", () => {
    const s = summarizeCleanup([])
    expect(s.allSucceeded).toBe(false)
    expect(s.unresolved).toHaveLength(0)
  })
})

describe("refusedStarts", () => {
  it("keeps only the members the backend refused, so a partial start shows", () => {
    const refused = refusedStarts([
      { task_id: 11, slot_index: 0, ok: true },
      { task_id: 12, slot_index: 1, ok: false, error: "task is not in todo" },
    ])
    expect(refused).toHaveLength(1)
    expect(refused[0].error).toBe("task is not in todo")
  })
})
