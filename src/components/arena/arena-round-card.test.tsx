import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

/**
 * The round card's contract with the user.
 *
 * The subject under test is not layout — it is **what the user is told after an
 * aggregate command**. The implementation this feature replaces reported cleanup
 * success while N complete repository copies stayed on disk, because the
 * per-member failures were collected by a `Promise.allSettled` and dropped. These
 * tests hold the replacement to the opposite standard: a success message appears
 * only when every member succeeded, and otherwise the backend's own reasons are
 * surfaced.
 */

const api = vi.hoisted(() => ({
  start: vi.fn(),
  cancel: vi.fn(),
  cleanup: vi.fn(),
}))

const toasts = vi.hoisted(() => ({
  success: vi.fn(),
  warning: vi.fn(),
  error: vi.fn(),
}))

vi.mock("@/lib/api", () => ({
  workTaskBatchStart: api.start,
  workTaskBatchCancel: api.cancel,
  workTaskBatchCleanup: api.cleanup,
}))

vi.mock("sonner", () => ({ toast: toasts }))

import enMessages from "@/i18n/messages/en.json"
import type { WorkTaskBatch, WorkTaskBatchMember } from "@/lib/types"
import { ArenaRoundCard } from "./arena-round-card"

function member(over: Partial<WorkTaskBatchMember> = {}): WorkTaskBatchMember {
  return {
    id: 1,
    batch_id: 1,
    task_id: 11,
    slot_index: 0,
    task_status: "review",
    task_title: "slot",
    label: "Claude Code",
    conversation_id: 5,
    connection_id: null,
    files_changed: 2,
    additions: 30,
    deletions: 4,
    ...over,
  }
}

function round(over: Partial<WorkTaskBatch> = {}): WorkTaskBatch {
  return {
    id: 1,
    folder_id: 7,
    title: "Rewrite the auth middleware",
    base_sha: "abcdef1234567890abcdef1234567890abcdef12",
    base_branch: "main",
    status: "review",
    failure_policy: "best_effort",
    max_concurrent: null,
    members: [
      member({ id: 1, task_id: 11, label: "Claude Code" }),
      member({ id: 2, task_id: 12, slot_index: 1, label: "Codex" }),
    ],
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    settled_at: null,
    ...over,
  }
}

function renderCard(over: Partial<WorkTaskBatch> = {}) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <ArenaRoundCard
        round={round(over)}
        folderName="my-project"
        onOpenTranscript={() => {}}
        onOpenDiff={() => {}}
      />
    </NextIntlClientProvider>
  )
}

beforeEach(() => {
  api.start.mockReset()
  api.cancel.mockReset()
  api.cleanup.mockReset()
  toasts.success.mockReset()
  toasts.warning.mockReset()
  toasts.error.mockReset()
})

describe("ArenaRoundCard", () => {
  /** The shared starting commit is the reason the results are comparable, so it
   *  is on the card rather than behind a detail view. */
  it("shows the pinned base commit the contenders share", () => {
    renderCard()
    expect(screen.getByText("main@abcdef1")).toBeTruthy()
  })

  it("names every contender", () => {
    renderCard()
    expect(screen.getByText("Claude Code")).toBeTruthy()
    expect(screen.getByText("Codex")).toBeTruthy()
  })

  it("reports a clean cleanup as a success", async () => {
    api.cleanup.mockResolvedValue([
      { task_id: 11, slot_index: 0, result: "succeeded" },
      { task_id: 12, slot_index: 1, result: "succeeded" },
    ])
    renderCard()

    await userEvent.click(screen.getByRole("button", { name: /Remove/ }))

    await waitFor(() => expect(toasts.success).toHaveBeenCalled())
    expect(toasts.warning).not.toHaveBeenCalled()
  })

  /**
   * The regression this whole design answers to. One member's removal was
   * refused; the card must NOT say the cleanup succeeded, and must pass on the
   * backend's reason.
   */
  it("never reports success when a member's cleanup was refused", async () => {
    api.cleanup.mockResolvedValue([
      { task_id: 11, slot_index: 0, result: "succeeded" },
      {
        task_id: 12,
        slot_index: 1,
        result: "failed",
        error: "worktree is locked by another process",
      },
    ])
    renderCard()

    await userEvent.click(screen.getByRole("button", { name: /Remove/ }))

    await waitFor(() => expect(toasts.warning).toHaveBeenCalled())
    expect(toasts.success).not.toHaveBeenCalled()
    const [, options] = toasts.warning.mock.calls[0] as [
      string,
      { description?: string },
    ]
    expect(options.description).toContain(
      "worktree is locked by another process"
    )
  })

  it("never reports success when a member's cleanup was blocked", async () => {
    api.cleanup.mockResolvedValue([
      { task_id: 11, slot_index: 0, result: "succeeded" },
      { task_id: 12, slot_index: 1, result: "blocked" },
    ])
    renderCard()

    await userEvent.click(screen.getByRole("button", { name: /Remove/ }))

    await waitFor(() => expect(toasts.warning).toHaveBeenCalled())
    expect(toasts.success).not.toHaveBeenCalled()
  })

  /** A start the backend partly refused is reported as partial, with the reason —
   *  telling the user a contender is running when it is not would send them
   *  waiting for output that never arrives. */
  it("reports a partial start as partial, naming the refusal", async () => {
    api.start.mockResolvedValue([
      { task_id: 11, slot_index: 0, ok: true },
      { task_id: 12, slot_index: 1, ok: false, error: "task is not in todo" },
    ])
    renderCard({
      status: "created",
      members: [
        member({ id: 1, task_id: 11, task_status: "todo" }),
        member({ id: 2, task_id: 12, slot_index: 1, task_status: "todo" }),
      ],
    })

    await userEvent.click(screen.getByRole("button", { name: "Start" }))

    await waitFor(() => expect(toasts.warning).toHaveBeenCalled())
    expect(toasts.success).not.toHaveBeenCalled()
    const [, options] = toasts.warning.mock.calls[0] as [
      string,
      { description?: string },
    ]
    expect(options.description).toContain("task is not in todo")
  })

  /** Worktrees survive a cancel exactly as they do for a single task; the card
   *  says so, or the user goes looking for work they think was destroyed. */
  it("says a cancel keeps the worktrees", async () => {
    api.cancel.mockResolvedValue([
      { task_id: 11, slot_index: 0, ok: true },
      { task_id: 12, slot_index: 1, ok: true },
    ])
    renderCard({
      status: "running",
      members: [
        member({ id: 1, task_id: 11, task_status: "running" }),
        member({ id: 2, task_id: 12, slot_index: 1, task_status: "running" }),
      ],
    })

    await userEvent.click(screen.getByRole("button", { name: /Cancel round/ }))

    await waitFor(() => expect(toasts.success).toHaveBeenCalled())
    const [, options] = toasts.success.mock.calls[0] as [
      string,
      { description?: string },
    ]
    expect(options.description).toMatch(/worktree/i)
  })

  /** Cleanup is not offered while a contender is live: the backend would refuse
   *  it and report `blocked`, so offering it invites a guaranteed partial. */
  it("withholds cleanup while a contender is still running", () => {
    renderCard({
      status: "running",
      members: [
        member({ id: 1, task_id: 11, task_status: "running" }),
        member({ id: 2, task_id: 12, slot_index: 1, task_status: "review" }),
      ],
    })
    expect(screen.queryByRole("button", { name: /Remove/ })).toBeNull()
  })

  /** A canceled round is not a launch pad — cancellation is a decision, and a new
   *  round would re-resolve the base anyway. */
  it("offers no start on a canceled round", () => {
    renderCard({
      status: "canceled",
      members: [
        member({ id: 1, task_id: 11, task_status: "canceled" }),
        member({ id: 2, task_id: 12, slot_index: 1, task_status: "canceled" }),
      ],
    })
    expect(screen.queryByRole("button", { name: "Start" })).toBeNull()
  })

  /** A cleanup failure lives on the member row, so it survives the window that
   *  asked for it. */
  it("keeps showing a member's unresolved cleanup failure", () => {
    renderCard({
      status: "settled",
      members: [
        member({
          id: 1,
          task_id: 11,
          task_status: "done",
          cleanup_result: "failed",
          cleanup_error: "worktree is locked",
        }),
      ],
    })
    expect(screen.getByText(/worktree is locked/)).toBeTruthy()
  })
})
