/**
 * The drawer's blocked panel.
 *
 * A to-do that cannot start says why (an unmet dependency, or the parent's run
 * / budget limit) and, for a dependency, offers the only way an edge is ever
 * removed — dependencies are never dropped automatically, so a failed upstream
 * would otherwise block the card forever.
 */
import { render, screen, waitFor, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import type { WorkTask } from "@/lib/types"

import { TaskDetailSheet } from "./task-detail-sheet"

const workTaskDependencyRemove = vi.fn().mockResolvedValue(true)

vi.mock("@/lib/api", () => ({
  workTaskArchive: vi.fn().mockResolvedValue(undefined),
  workTaskCancel: vi.fn().mockResolvedValue(undefined),
  getFolderConversation: vi.fn().mockRejectedValue(new Error("no transcript")),
  workTaskChangedFiles: vi.fn().mockResolvedValue([]),
  workTaskCleanup: vi.fn().mockResolvedValue(undefined),
  workTaskDelete: vi.fn().mockResolvedValue(undefined),
  workTaskDependencyRemove: (...args: unknown[]) =>
    workTaskDependencyRemove(...args),
  workTaskDiff: vi.fn().mockResolvedValue(""),
  workTaskEvents: vi.fn().mockResolvedValue([]),
  workTaskMergeUnqueue: vi.fn().mockResolvedValue(undefined),
  workTaskRequeue: vi.fn().mockResolvedValue(undefined),
  workTaskRetry: vi.fn().mockResolvedValue(undefined),
  workTaskReturn: vi.fn().mockResolvedValue(undefined),
  workTaskStart: vi.fn().mockResolvedValue(undefined),
}))
vi.mock("@/lib/platform", () => ({
  subscribe: vi.fn().mockResolvedValue(() => {}),
  onTransportReconnect: vi.fn(() => () => {}),
}))
vi.mock("@/stores/app-workspace-store", () => {
  const state = {
    allFolders: [{ id: 1, path: "/repo", default_agent_type: null }],
  }
  const useStore = (selector: (s: typeof state) => unknown) => selector(state)
  return { useAppWorkspaceStore: useStore }
})
vi.mock("@/components/ai-elements/message", () => ({
  MessageResponse: ({ children }: { children?: string }) => (
    <div>{children}</div>
  ),
}))
vi.mock("@/components/diff/unified-diff-preview", () => ({
  UnifiedDiffPreview: () => <div />,
}))
vi.mock("./task-message-composer", () => ({
  TaskMessageComposer: () => <div data-testid="follow-up-composer" />,
}))
vi.mock("./task-transcript-dialog", () => ({
  TaskTranscriptDialog: () => null,
}))

function task(overrides: Partial<WorkTask> = {}): WorkTask {
  return {
    id: 7,
    folder_id: 1,
    title: "Integrate the pieces",
    config: null,
    status: "todo",
    work_branch: null,
    worktree_folder_id: null,
    conversation_id: null,
    archived_at: null,
    scheduled_at: null,
    cleanup_state: null,
    preflight: null,
    files_changed: 0,
    blocked: null,
    ...overrides,
  } as WorkTask
}

function mount(row: WorkTask) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <TaskDetailSheet
        open
        onOpenChange={() => {}}
        task={row}
        folderName="repo"
        onMerge={() => {}}
        onComplete={() => {}}
        onDeliverPr={() => {}}
        onCancel={() => {}}
        onEdit={() => {}}
        onSchedule={() => {}}
      />
    </NextIntlClientProvider>
  )
}

beforeEach(() => {
  vi.clearAllMocks()
})

describe("task drawer blocked panel", () => {
  it("says nothing when the task is not blocked", () => {
    mount(task())
    expect(screen.queryByText("Waiting to start")).not.toBeInTheDocument()
  })

  it("names the unmet dependency with its live status", async () => {
    mount(
      task({
        blocked: {
          reason: "dependency",
          dependencies: [
            { task_id: 11, title: "Write the schema", status: "failed" },
          ],
        },
      })
    )
    expect(await screen.findByText("Waiting to start")).toBeInTheDocument()
    expect(
      screen.getByText("Waiting for 1 earlier task(s) to finish first.")
    ).toBeInTheDocument()
    expect(screen.getByText("Write the schema")).toBeInTheDocument()
    expect(screen.getByText("#11")).toBeInTheDocument()
    expect(screen.getByText("Failed")).toBeInTheDocument()
  })

  it("removes the edge when its chip's ✕ is clicked", async () => {
    const user = userEvent.setup()
    mount(
      task({
        blocked: {
          reason: "dependency",
          dependencies: [
            { task_id: 11, title: "Write the schema", status: "canceled" },
            { task_id: 12, title: "Wire the API", status: "todo" },
          ],
        },
      })
    )

    const removes = await screen.findAllByRole("button", {
      name: "Remove this dependency",
    })
    expect(removes).toHaveLength(2)
    // The first chip's ✕ names the edge it removes — the second is untouched.
    await user.click(removes[0])
    await waitFor(() =>
      expect(workTaskDependencyRemove).toHaveBeenCalledWith(7, 11)
    )
  })

  it("reports a parent limit without inventing a chip", async () => {
    mount(
      task({
        blocked: {
          reason: "runs",
          detail: "2/2 sibling subtasks are already running",
        },
      })
    )
    expect(
      await screen.findByText(
        "This subtask has reached the parent task's run limit."
      )
    ).toBeInTheDocument()
    expect(
      screen.getByText("2/2 sibling subtasks are already running")
    ).toBeInTheDocument()
    expect(
      screen.queryByRole("button", { name: "Remove this dependency" })
    ).not.toBeInTheDocument()
  })

  it("renders one chip per unmet dependency in board order", async () => {
    mount(
      task({
        blocked: {
          reason: "dependency",
          dependencies: [
            { task_id: 11, title: "Write the schema", status: "done" },
            { task_id: 12, title: "Wire the API", status: "todo" },
          ],
        },
      })
    )
    const chips = await screen.findAllByRole("listitem")
    const labels = chips.map(
      (chip) => within(chip).getByText(/#\d+/).textContent
    )
    expect(labels).toEqual(["#11", "#12"])
  })
})
