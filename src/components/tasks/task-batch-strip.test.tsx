import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { describe, expect, it, vi } from "vitest"

/**
 * The strip's dismissal wiring — the exit for a group that is over and fully
 * cleaned. The gating rule itself lives in `work-task-batch-model` and is
 * tested there; what this pins is that the button appears exactly when the
 * model allows it and that pressing it soft-deletes the grouping (and nothing
 * else — the members are not touched).
 */

const api = vi.hoisted(() => ({
  cancel: vi.fn(),
  cleanup: vi.fn(),
  del: vi.fn(),
}))
const toasts = vi.hoisted(() => ({
  success: vi.fn(),
  warning: vi.fn(),
  error: vi.fn(),
}))

vi.mock("@/lib/api", () => ({
  workTaskBatchCancel: api.cancel,
  workTaskBatchCleanup: api.cleanup,
  workTaskBatchDelete: api.del,
}))
vi.mock("sonner", () => ({ toast: toasts }))

import enMessages from "@/i18n/messages/en.json"
import type { WorkTaskBatch, WorkTaskBatchMember } from "@/lib/types"
import { TaskBatchStrip } from "./task-batch-strip"

function member(over: Partial<WorkTaskBatchMember> = {}): WorkTaskBatchMember {
  return {
    id: 1,
    batch_id: 1,
    task_id: 11,
    slot_index: 0,
    task_status: "done",
    task_title: "slot",
    conversation_id: null,
    connection_id: null,
    files_changed: null,
    additions: null,
    deletions: null,
    ...over,
  }
}

function batch(over: Partial<WorkTaskBatch> = {}): WorkTaskBatch {
  return {
    id: 1,
    folder_id: 7,
    title: "Friday cleanup pass",
    base_sha: "a".repeat(40),
    base_branch: "main",
    status: "settled",
    failure_policy: "best_effort",
    members: [
      member({ task_id: 11, cleanup_result: "succeeded" }),
      member({
        id: 2,
        task_id: 12,
        slot_index: 1,
        cleanup_result: "succeeded",
      }),
    ],
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    settled_at: "2026-01-02T00:00:00Z",
    ...over,
  }
}

function renderStrip(batches: WorkTaskBatch[]) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <TaskBatchStrip batches={batches} folderNames={new Map()} />
    </NextIntlClientProvider>
  )
}

describe("TaskBatchStrip dismissal", () => {
  it("offers removal for an over batch with nothing left to clean", async () => {
    api.del.mockResolvedValue(undefined)
    renderStrip([batch()])
    const user = userEvent.setup()

    await user.click(screen.getByRole("button", { name: "Remove from list" }))

    await waitFor(() => expect(api.del).toHaveBeenCalledWith(1))
    // A soft delete needs no toast: the backend's Deleted event makes every
    // listener refetch, and the row unmounts with that refetch.
    expect(toasts.success).not.toHaveBeenCalled()
  })

  it("hides removal while a member is still cleanable", () => {
    renderStrip([
      batch({
        members: [
          member({ task_id: 11, cleanup_result: "succeeded" }),
          member({ id: 2, task_id: 12, slot_index: 1 }),
        ],
      }),
    ])
    expect(
      screen.queryByRole("button", { name: "Remove from list" })
    ).not.toBeInTheDocument()
    // Cleanup is what is offered instead.
    expect(
      screen.getByRole("button", { name: /Clean up 1/ })
    ).toBeInTheDocument()
  })
})
