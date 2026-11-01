import { render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import enMessages from "@/i18n/messages/en.json"
import type { PendingPermissionState, WorkTask } from "@/lib/types"
import {
  acpGetSessionSnapshotByConversation,
  acpRespondPermission,
} from "@/lib/api"
import {
  TaskPendingApproval,
  useWorkTaskPendingApproval,
} from "./task-pending-approval"

vi.mock("@/lib/api", () => ({
  acpGetSessionSnapshotByConversation: vi.fn(async () => null),
  acpRespondPermission: vi.fn(async () => {}),
}))

const pull = vi.mocked(acpGetSessionSnapshotByConversation)
const respond = vi.mocked(acpRespondPermission)

// `useAgentVocabulary` reads next-intl; the card is rendered under the same
// provider the app gives it. Mock call counts are per-test.
function withIntl(ui: React.ReactElement) {
  return (
    <NextIntlClientProvider locale="en" messages={enMessages}>
      {ui}
    </NextIntlClientProvider>
  )
}

beforeEach(() => {
  pull.mockClear()
  respond.mockClear()
})

function permission(
  overrides?: Partial<PendingPermissionState>
): PendingPermissionState {
  return {
    request_id: "req-1",
    tool_call_id: "call-1",
    tool_call: {
      title: "Bash(ls | grep r31)",
      kind: "execute",
      rawInput: { command: "ls | grep r31" },
    },
    options: [
      { option_id: "allow", name: "Allow", kind: "allow_once", meta: null },
      { option_id: "reject", name: "Deny", kind: "reject_once", meta: null },
    ],
    created_at: "2026-10-31T03:20:24Z",
    queued: 0,
    ...overrides,
  }
}

function task(overrides?: Partial<WorkTask>): WorkTask {
  return {
    id: 51,
    folder_id: 1,
    title: "R31-09",
    config: null,
    status: "awaiting_input",
    failure_reason: null,
    last_error: null,
    run_seq: 1,
    sort_order: 1,
    worktree_folder_id: null,
    conversation_id: 56,
    connection_id: "conn-56",
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
    deferred_at: null,
    source_kind: null,
    source_key: null,
    source_meta: null,
    parent_id: null,
    ...overrides,
  } as WorkTask
}

/** The card alone: renders what the run asks and answers it in place. */
function Card() {
  return (
    <TaskPendingApproval
      task={task({ agent_type: "codex" })}
      pending={permission()}
    />
  )
}

describe("TaskPendingApproval", () => {
  it("shows the command the agent wants to run and the agent's own options", () => {
    render(withIntl(<Card />))
    expect(screen.getByText("ls | grep r31")).toBeInTheDocument()
    expect(screen.getByRole("button", { name: "Allow" })).toBeInTheDocument()
    expect(screen.getByRole("button", { name: "Deny" })).toBeInTheDocument()
  })

  it("answers through the shared respond command with the run's connection", async () => {
    render(withIntl(<Card />))
    await userEvent.click(screen.getByRole("button", { name: "Allow" }))
    expect(respond).toHaveBeenCalledWith("conn-56", "req-1", "allow")
  })

  it("does not stop at disabled buttons when the connection is gone", () => {
    render(
      withIntl(
        <TaskPendingApproval
          task={task({ connection_id: null, agent_type: "codex" })}
          pending={permission()}
        />
      )
    )
    expect(screen.getByRole("button", { name: "Allow" })).toBeDisabled()
  })
})

describe("useWorkTaskPendingApproval", () => {
  function Probe({ t }: { t: WorkTask }) {
    const pending = useWorkTaskPendingApproval(t)
    return pending ? <div data-testid="asked">{pending.request_id}</div> : null
  }

  it("pulls the run conversation's pending permission while awaiting input", async () => {
    pull.mockResolvedValueOnce({
      pending_permission: permission(),
    } as Awaited<ReturnType<typeof pull>>)
    render(withIntl(<Probe t={task()} />))
    expect(await screen.findByTestId("asked")).toHaveTextContent("req-1")
    expect(pull).toHaveBeenCalledWith(56)
  })

  it("renders nothing when the task is not awaiting input", async () => {
    const running = task({ status: "running" })
    render(withIntl(<Probe t={running} />))
    // Give a stray poll a chance; none may happen for a non-awaiting task.
    await Promise.resolve()
    expect(screen.queryByTestId("asked")).not.toBeInTheDocument()
    expect(pull).not.toHaveBeenCalled()
  })

  it("survives a failed pull without rendering a stale request", async () => {
    pull.mockRejectedValueOnce(new Error("connection gone"))
    render(withIntl(<Probe t={task()} />))
    // First poll errors → nothing renders; a later snapshot can still arrive.
    await new Promise((r) => setTimeout(r, 0))
    expect(screen.queryByTestId("asked")).not.toBeInTheDocument()
  })
})
