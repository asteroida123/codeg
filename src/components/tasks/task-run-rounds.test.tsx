/**
 * The drawer's "rounds" section: one row per execution generation, and the
 * banner that owns the one remedy of a strict-continuation refusal. The point
 * these pin is that the round's own evidence is visible — kind, how the
 * previous session was continued, cost — and that `resume_failed` is not
 * presented as just another failure.
 */
import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import enMessages from "@/i18n/messages/en.json"
import type { WorkTask, WorkTaskDelegation, WorkTaskRun } from "@/lib/types"

const workTaskRuns = vi.fn().mockResolvedValue([])
const taskDelegations = vi.fn().mockResolvedValue([])

vi.mock("@/lib/api", () => ({
  workTaskRuns: (...args: unknown[]) => workTaskRuns(...args),
  taskDelegations: (...args: unknown[]) => taskDelegations(...args),
  getFolderConversation: vi.fn().mockRejectedValue(new Error("no transcript")),
}))

vi.mock("@/lib/platform", () => ({
  subscribe: vi.fn().mockResolvedValue(() => {}),
  onTransportReconnect: vi.fn(() => () => {}),
}))

import { TaskRunRounds } from "./task-run-rounds"
import { TaskDelegationsList } from "./task-delegations-list"

function task(overrides: Partial<WorkTask> = {}): WorkTask {
  return {
    id: 7,
    folder_id: 1,
    title: "Fix login",
    config: null,
    status: "failed",
    failure_reason: null,
    run_seq: 2,
    ...overrides,
  } as WorkTask
}

function run(overrides: Partial<WorkTaskRun> = {}): WorkTaskRun {
  return {
    id: 1,
    task_id: 7,
    run_seq: 2,
    kind: "retry",
    status: "settled",
    resume_outcome: "resumed",
    started_at: "2026-09-26T00:00:00Z",
    ...overrides,
  } as WorkTaskRun
}

function renderRounds(row: WorkTask, onRunWithNewSession?: () => void) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <TaskRunRounds
        open
        task={row}
        onRunWithNewSession={onRunWithNewSession}
      />
    </NextIntlClientProvider>
  )
}

beforeEach(() => {
  workTaskRuns.mockReset().mockResolvedValue([])
  taskDelegations.mockReset().mockResolvedValue([])
})

describe("TaskRunRounds", () => {
  it("names each round's kind, continuation and cost", async () => {
    workTaskRuns.mockResolvedValue([
      run({ id: 2, run_seq: 2, resume_outcome: "resumed", duration_ms: 4_000 }),
      run({
        id: 1,
        run_seq: 1,
        kind: "fresh",
        status: "failed",
        resume_outcome: "fresh_no_session",
        error_code: "setup_error",
      }),
    ])
    renderRounds(task())

    await waitFor(() => expect(screen.getByText("Round 2")).toBeInTheDocument())
    expect(screen.getByText("Retry")).toBeInTheDocument()
    expect(screen.getByText("Continued session")).toBeInTheDocument()
    expect(screen.getByText("4s")).toBeInTheDocument()
    expect(screen.getByText("Round 1")).toBeInTheDocument()
    expect(screen.getByText("First run")).toBeInTheDocument()
    expect(screen.getByText("Error: setup_error")).toBeInTheDocument()
  })

  it("labels a session that could not be continued as its own outcome", async () => {
    workTaskRuns.mockResolvedValue([
      run({ status: "failed", resume_outcome: "strict_failed" }),
    ])
    renderRounds(task())

    await waitFor(() =>
      expect(
        screen.getByText("Previous session could not be continued")
      ).toBeInTheDocument()
    )
  })

  it("offers the new-session remedy only when the failure is resume_failed", async () => {
    const onRunWithNewSession = vi.fn()
    const { rerender } = renderRounds(
      task({ failure_reason: "agent_error" }),
      onRunWithNewSession
    )
    expect(
      screen.queryByRole("button", { name: "Run with a new session" })
    ).not.toBeInTheDocument()

    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <TaskRunRounds
          open
          task={task({ failure_reason: "resume_failed" })}
          onRunWithNewSession={onRunWithNewSession}
        />
      </NextIntlClientProvider>
    )
    const user = userEvent.setup()
    const button = await screen.findByRole("button", {
      name: "Run with a new session",
    })
    expect(
      screen.getByText("The previous session could not be continued")
    ).toBeInTheDocument()
    await user.click(button)
    expect(onRunWithNewSession).toHaveBeenCalledTimes(1)
  })
})

describe("TaskDelegationsList", () => {
  it("lists the delegated runs with their status and session link", async () => {
    taskDelegations.mockResolvedValue([
      {
        task_id: "d1",
        agent_type: "codex",
        status: "completed",
        task: "audit the retry path",
        child_conversation_id: 42,
        created_at: "2026-09-26T00:00:00Z",
        updated_at: "2026-09-26T00:01:00Z",
        effective_model: "gpt-5",
      } as WorkTaskDelegation,
    ])
    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <TaskDelegationsList open task={task({ status: "running" })} />
      </NextIntlClientProvider>
    )

    await waitFor(() =>
      expect(screen.getByText("audit the retry path")).toBeInTheDocument()
    )
    expect(screen.getByText("Completed")).toBeInTheDocument()
    expect(screen.getByText("gpt-5")).toBeInTheDocument()
    expect(
      screen.getByRole("button", { name: /open session/i })
    ).toBeInTheDocument()
  })
})
