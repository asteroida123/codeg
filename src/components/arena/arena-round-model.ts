import type {
  BatchCleanupOutcome,
  WorkTaskBatch,
  WorkTaskBatchMember,
  WorkTaskStatus,
} from "@/lib/types"

/**
 * Pure presentation logic for a comparison round. Kept out of the components so
 * the rules that matter — which controls are offered, and what a cleanup
 * actually achieved — are testable without rendering anything.
 */

/** How a member reads on the grid. A presentation grouping over the task
 *  statuses, not a second status set: the task row remains the truth. */
export type MemberPhase =
  /** Not started, or queued behind the folder's concurrency limit. */
  | "waiting"
  /** Setting up or working. */
  | "working"
  /** Parked on a question, a permission, or a plan approval. */
  | "blocked"
  /** Finished; its result is there to compare. */
  | "finished"
  /** Failed or was canceled. */
  | "stopped"

export function memberPhase(status: WorkTaskStatus | null): MemberPhase {
  switch (status) {
    case "todo":
    case "queued":
      return "waiting"
    case "preparing":
    case "running":
    case "merging":
      return "working"
    case "awaiting_input":
      return "blocked"
    case "review":
    case "done":
      return "finished"
    case "failed":
    case "canceled":
      return "stopped"
    // A member whose task row is gone. Reads as stopped rather than waiting: it
    // will never report again, and offering it a start would be a lie.
    case null:
    default:
      return "stopped"
  }
}

/** Which aggregate controls a round can be offered, and why not. */
export interface RoundControls {
  /** At least one member is startable (never launched, or failed and retryable). */
  canStart: boolean
  /** At least one member is still cancelable. */
  canCancel: boolean
  /**
   * Cleanup is offered only when nothing is live. The backend refuses a live
   * member anyway and reports it `blocked` — this just avoids offering an action
   * that is guaranteed to be partly refused.
   */
  canCleanup: boolean
  /** Members that still hold a worktree the user may want removed. */
  cleanableCount: number
}

export function roundControls(round: WorkTaskBatch): RoundControls {
  const phases = round.members.map((m) => memberPhase(m.task_status))
  const anyLive = phases.some((p) => p === "working" || p === "blocked")
  const canStart =
    round.status !== "canceled" &&
    round.members.some((m) => {
      const phase = memberPhase(m.task_status)
      // `waiting` covers never-started; `stopped` covers a failure worth
      // retrying. A canceled member is `stopped` too and the backend will
      // refuse it — reported per member rather than hidden here.
      return phase === "waiting" || phase === "stopped"
    })
  const cleanableCount = round.members.filter(
    (m) => m.cleanup_result !== "succeeded"
  ).length

  return {
    canStart,
    canCancel: anyLive || phases.some((p) => p === "waiting"),
    canCleanup: !anyLive && cleanableCount > 0,
    cleanableCount,
  }
}

/** Total diff size across a round's members — the headline "how much did this
 *  round produce". `null` counts as 0: a member with no worktree produced
 *  nothing measurable, which is different from producing nothing at all but not
 *  in a way a total can express. */
export function roundDiffTotals(round: WorkTaskBatch): {
  filesChanged: number
  additions: number
  deletions: number
} {
  return round.members.reduce(
    (acc, m) => ({
      filesChanged: acc.filesChanged + (m.files_changed ?? 0),
      additions: acc.additions + (m.additions ?? 0),
      deletions: acc.deletions + (m.deletions ?? 0),
    }),
    { filesChanged: 0, additions: 0, deletions: 0 }
  )
}

/** A member's display name: its own label, or the task title as a fallback. */
export function memberLabel(member: WorkTaskBatchMember): string {
  const label = member.label?.trim()
  return label && label.length > 0 ? label : member.task_title
}

/**
 * What to tell the user after an aggregate cleanup.
 *
 * Deliberately NOT reducible to "done". The review this whole feature answers to
 * found a UI that reported cleanup success while N complete repository copies
 * stayed on disk, because the per-member failures had been collected by a
 * `Promise.allSettled` and dropped. So this returns the counts AND keeps the
 * failed members, and `allSucceeded` is only true when every single one did.
 */
export interface CleanupSummary {
  succeeded: number
  failed: number
  blocked: number
  /** Every attempted member succeeded. The ONLY case a plain success message is
   *  honest. */
  allSucceeded: boolean
  /** The members that did not, with the backend's reason — what a retry needs. */
  unresolved: BatchCleanupOutcome[]
}

export function summarizeCleanup(
  outcomes: BatchCleanupOutcome[]
): CleanupSummary {
  const succeeded = outcomes.filter((o) => o.result === "succeeded").length
  const failed = outcomes.filter((o) => o.result === "failed").length
  const blocked = outcomes.filter((o) => o.result === "blocked").length
  return {
    succeeded,
    failed,
    blocked,
    // An empty result set is NOT a success: nothing was attempted, so claiming
    // success would describe work that never happened.
    allSucceeded: outcomes.length > 0 && failed === 0 && blocked === 0,
    unresolved: outcomes.filter((o) => o.result !== "succeeded"),
  }
}

/** Members whose start the backend refused, for the same reason as above: a
 *  partial start must not read as a whole one. */
export function refusedStarts<T extends { ok: boolean }>(outcomes: T[]): T[] {
  return outcomes.filter((o) => !o.ok)
}
