import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

/**
 * The batch dialog's job is to only ever offer a selection the backend will
 * accept.
 *
 * Every rule here mirrors a refusal in `work_task_batch_service::adopt`. Enforcing
 * them in the picker is not duplication for its own sake — it is the difference
 * between the user learning the rule from the list and learning it from an error
 * after choosing. The backend remains the authority; it re-checks all of them.
 */

const api = vi.hoisted(() => ({
  adopt: vi.fn(),
  start: vi.fn(),
}))
const toasts = vi.hoisted(() => ({
  success: vi.fn(),
  warning: vi.fn(),
  error: vi.fn(),
}))

vi.mock("@/lib/api", () => ({
  workTaskBatchAdopt: api.adopt,
  workTaskBatchStart: api.start,
}))
vi.mock("sonner", () => ({ toast: toasts }))

import enMessages from "@/i18n/messages/en.json"
import type { WorkTask } from "@/lib/types"
import { TaskBatchDialog } from "./task-batch-dialog"

function task(over: Partial<WorkTask> = {}): WorkTask {
  return {
    id: 1,
    folder_id: 7,
    title: "a task",
    config: null,
    status: "todo",
    failure_reason: null,
    last_error: null,
    run_seq: 0,
    sort_order: 0,
    worktree_folder_id: null,
    conversation_id: null,
    connection_id: null,
    base_branch: null,
    base_sha: null,
    work_branch: null,
    cleanup_state: null,
    verdict: null,
    result_summary: null,
    files_changed: null,
    additions: null,
    deletions: null,
    merge_commit: null,
    preflight: null,
    merge_queued: null,
    archived_at: null,
    scheduled_at: null,
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    started_at: null,
    settled_at: null,
    finished_at: null,
    ...over,
  } as WorkTask
}

const FOLDER_NAMES = new Map([
  [7, "my-project"],
  [8, "other-project"],
])

function renderDialog(tasks: WorkTask[]) {
  const onCreated = vi.fn()
  const result = render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <TaskBatchDialog
        open
        onOpenChange={() => {}}
        tasks={tasks}
        folderNames={FOLDER_NAMES}
        onCreated={onCreated}
      />
    </NextIntlClientProvider>
  )
  return { ...result, onCreated }
}

beforeEach(() => {
  api.adopt.mockReset()
  api.start.mockReset()
  toasts.success.mockReset()
  toasts.warning.mockReset()
  toasts.error.mockReset()
  api.adopt.mockResolvedValue({ id: 99 })
  api.start.mockResolvedValue([
    { task_id: 1, slot_index: 0, ok: true },
    { task_id: 2, slot_index: 1, ok: true },
  ])
})

describe("TaskBatchDialog eligibility", () => {
  it("offers only to-do tasks", () => {
    renderDialog([
      task({ id: 1, title: "still to do" }),
      task({ id: 2, title: "already running", status: "running" }),
      task({ id: 3, title: "in review", status: "review" }),
      task({ id: 4, title: "finished", status: "done" }),
    ])
    expect(screen.getByText("still to do")).toBeTruthy()
    expect(screen.queryByText("already running")).toBeNull()
    expect(screen.queryByText("in review")).toBeNull()
    expect(screen.queryByText("finished")).toBeNull()
  })

  /** A task with a worktree already started somewhere, so a batch claiming to pin
   *  its base would record a commit it does not have. */
  it("withholds a task that already has a worktree", () => {
    renderDialog([
      task({ id: 1, title: "fresh" }),
      task({ id: 2, title: "has a worktree", worktree_folder_id: 42 }),
    ])
    expect(screen.getByText("fresh")).toBeTruthy()
    expect(screen.queryByText("has a worktree")).toBeNull()
  })

  it("withholds an archived task", () => {
    renderDialog([
      task({ id: 1, title: "visible" }),
      task({ id: 2, title: "archived", archived_at: "2026-01-01T00:00:00Z" }),
    ])
    expect(screen.getByText("visible")).toBeTruthy()
    expect(screen.queryByText("archived")).toBeNull()
  })

  it("says so when nothing is eligible, rather than showing an empty list", () => {
    renderDialog([task({ id: 1, status: "done" })])
    expect(screen.getByText(/No eligible to-dos/)).toBeTruthy()
  })

  /**
   * A batch shares one repository's commit. Once a project is chosen by the first
   * tick, the other projects' rows are disabled — the constraint becomes a visible
   * property of the list instead of an error after choosing.
   */
  it("locks the selection to one project after the first tick", async () => {
    renderDialog([
      task({ id: 1, folder_id: 7, title: "mine one" }),
      task({ id: 2, folder_id: 7, title: "mine two" }),
      task({ id: 3, folder_id: 8, title: "theirs" }),
    ])
    const boxes = screen.getAllByRole("checkbox")
    // Nothing picked yet: everything is available.
    expect(boxes.every((b) => !(b as HTMLInputElement).disabled)).toBe(true)

    await userEvent.click(boxes[0])

    // The other project's row is now unavailable; the same project's is not.
    const after = screen.getAllByRole("checkbox") as HTMLInputElement[]
    expect(after[1].disabled).toBe(false)
    expect(after[2].disabled).toBe(true)
  })

  /** A group of one is just the task; its extra machinery would buy nothing. */
  it("does not submit a single task", async () => {
    renderDialog([task({ id: 1, title: "one" }), task({ id: 2, title: "two" })])
    await userEvent.type(screen.getByLabelText("Group name"), "Friday cleanup")
    await userEvent.click(screen.getAllByRole("checkbox")[0])

    const run = screen.getByRole("button", { name: /Run/ })
    expect((run as HTMLButtonElement).disabled).toBe(true)

    await userEvent.click(screen.getAllByRole("checkbox")[1])
    expect((run as HTMLButtonElement).disabled).toBe(false)
  })

  it("requires a name", async () => {
    renderDialog([task({ id: 1, title: "one" }), task({ id: 2, title: "two" })])
    await userEvent.click(screen.getAllByRole("checkbox")[0])
    await userEvent.click(screen.getAllByRole("checkbox")[1])
    expect(
      (screen.getByRole("button", { name: /Run/ }) as HTMLButtonElement)
        .disabled
    ).toBe(true)
  })
})

describe("TaskBatchDialog submission", () => {
  async function pickTwoAndRun() {
    await userEvent.type(screen.getByLabelText("Group name"), "Friday cleanup")
    await userEvent.click(screen.getAllByRole("checkbox")[0])
    await userEvent.click(screen.getAllByRole("checkbox")[1])
    await userEvent.click(screen.getByRole("button", { name: /Run/ }))
  }

  it("adopts the existing tasks, then starts the group", async () => {
    const { onCreated } = renderDialog([
      task({ id: 1, folder_id: 7, title: "one" }),
      task({ id: 2, folder_id: 7, title: "two" }),
    ])
    await pickTwoAndRun()

    await waitFor(() => expect(api.adopt).toHaveBeenCalled())
    // Adopt, not create: these tasks already exist and must not be duplicated.
    expect(api.adopt).toHaveBeenCalledWith(7, "Friday cleanup", [1, 2])
    expect(api.start).toHaveBeenCalledWith(99)
    expect(onCreated).toHaveBeenCalled()
    expect(toasts.success).toHaveBeenCalled()
  })

  /** Board order, not click order: the slots should match what the user sees. */
  it("sends the members in board order regardless of click order", async () => {
    renderDialog([
      task({ id: 1, folder_id: 7, title: "one" }),
      task({ id: 2, folder_id: 7, title: "two" }),
      task({ id: 3, folder_id: 7, title: "three" }),
    ])
    await userEvent.type(screen.getByLabelText("Group name"), "g")
    const boxes = screen.getAllByRole("checkbox")
    await userEvent.click(boxes[2])
    await userEvent.click(boxes[0])
    await userEvent.click(screen.getByRole("button", { name: /Run/ }))

    await waitFor(() => expect(api.adopt).toHaveBeenCalled())
    expect(api.adopt).toHaveBeenCalledWith(7, "g", [1, 3])
  })

  /** A partial start is reported as partial, with the backend's reason. */
  it("reports a partly-refused start as partial", async () => {
    api.start.mockResolvedValue([
      { task_id: 1, slot_index: 0, ok: true },
      { task_id: 2, slot_index: 1, ok: false, error: "task is not in todo" },
    ])
    renderDialog([
      task({ id: 1, folder_id: 7, title: "one" }),
      task({ id: 2, folder_id: 7, title: "two" }),
    ])
    await pickTwoAndRun()

    await waitFor(() => expect(toasts.warning).toHaveBeenCalled())
    expect(toasts.success).not.toHaveBeenCalled()
    const [, options] = toasts.warning.mock.calls[0] as [
      string,
      { description?: string },
    ]
    expect(options.description).toContain("task is not in todo")
  })

  /** The backend names the task and the reason; pass it through rather than
   *  replacing it with a generic failure. */
  it("surfaces the backend's refusal verbatim", async () => {
    api.adopt.mockRejectedValue(
      new Error(
        "task 2 already has a worktree, so its starting commit is already decided"
      )
    )
    renderDialog([
      task({ id: 1, folder_id: 7, title: "one" }),
      task({ id: 2, folder_id: 7, title: "two" }),
    ])
    await pickTwoAndRun()

    await waitFor(() => expect(toasts.error).toHaveBeenCalled())
    const [, options] = toasts.error.mock.calls[0] as [
      string,
      { description?: string },
    ]
    expect(options.description).toContain("already has a worktree")
    expect(api.start).not.toHaveBeenCalled()
  })
})
