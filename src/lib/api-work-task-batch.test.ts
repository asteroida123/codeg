import { beforeEach, describe, expect, it, vi } from "vitest"

/**
 * The batch client's contract with the backend.
 *
 * A batch groups existing work tasks over one immutable base commit. The client
 * is deliberately thin: it sends commands and reads back per-member answers. The
 * backend stays the single execution authority, which is what makes closing this
 * window — or opening a second one — leave a running batch untouched.
 *
 * These tests pin the two things a thin client can still get wrong, both of
 * which fail silently at runtime rather than at compile time:
 *
 * 1. **Command names and parameter casing.** A typo reaches the transport as a
 *    404 / unknown-command at the moment the user clicks, not at build time.
 * 2. **Per-member answers stay per member.** The aggregate calls return an array
 *    with one entry per member, and a caller must not be able to mistake it for
 *    a single verdict. The reviewed implementation this design replaces reported
 *    one cleanup success while every worktree stayed on disk, because the
 *    per-member failures were collected and dropped.
 */

const mocks = vi.hoisted(() => ({
  call: vi.fn(),
}))

vi.mock("@/lib/transport", () => ({
  getTransport: () => ({ call: mocks.call }),
  getShellTransport: () => ({ call: vi.fn() }),
  isDesktop: () => false,
  isRemoteDesktopMode: () => false,
  getActiveRemoteConnectionId: () => null,
  notifyRemoteDesktopUnauthorized: vi.fn(),
}))

import {
  workTaskBatchCancel,
  workTaskBatchCleanup,
  workTaskBatchCreate,
  workTaskBatchDelete,
  workTaskBatchGet,
  workTaskBatchList,
  workTaskBatchStart,
} from "@/lib/api"
import type {
  BatchCleanupOutcome,
  BatchMemberOutcome,
  WorkTaskBatchSpec,
} from "@/lib/types"

function spec(memberCount: number): WorkTaskBatchSpec {
  return {
    folder_id: 7,
    title: "compare three agents",
    members: Array.from({ length: memberCount }, (_, i) => ({
      title: `slot ${i}`,
      config: {
        prompt_blocks: [{ type: "text", text: "do the thing" }],
        display_text: "do the thing",
        config_values: {},
      },
    })),
  }
}

/** The most recent transport call. `mock.calls.at(-1)` would be cleaner, but the
 *  project's TS lib target predates `Array.prototype.at`. */
function lastCall(): [string, Record<string, unknown>] {
  const calls = mocks.call.mock.calls
  return calls[calls.length - 1] as [string, Record<string, unknown>]
}

beforeEach(() => {
  mocks.call.mockReset()
  mocks.call.mockResolvedValue(undefined)
})

describe("work task batch client", () => {
  it("addresses each backend command by its registered name", async () => {
    await workTaskBatchList(7)
    expect(mocks.call).toHaveBeenLastCalledWith("work_task_batch_list", {
      folderId: 7,
    })

    // No folder = every folder, sent explicitly as null rather than omitted:
    // the handler's `Option<i32>` reads an absent key and a null the same way,
    // but only one of them survives a JSON body round-trip unambiguously.
    await workTaskBatchList()
    expect(mocks.call).toHaveBeenLastCalledWith("work_task_batch_list", {
      folderId: null,
    })

    await workTaskBatchGet(3)
    expect(mocks.call).toHaveBeenLastCalledWith("work_task_batch_get", {
      id: 3,
    })

    await workTaskBatchStart(3)
    expect(mocks.call).toHaveBeenLastCalledWith("work_task_batch_start", {
      id: 3,
    })

    await workTaskBatchCancel(3)
    expect(mocks.call).toHaveBeenLastCalledWith("work_task_batch_cancel", {
      id: 3,
    })

    await workTaskBatchDelete(3)
    expect(mocks.call).toHaveBeenLastCalledWith("work_task_batch_delete", {
      id: 3,
    })
  })

  it("sends the whole spec, with every member's config, under `spec`", async () => {
    await workTaskBatchCreate(spec(3))

    const [command, params] = lastCall()
    const sent = (params as { spec: WorkTaskBatchSpec }).spec
    expect(command).toBe("work_task_batch_create")
    expect(sent.folder_id).toBe(7)
    expect(sent.title).toBe("compare three agents")
    expect(sent.members).toHaveLength(3)
    // Member order is the slot order the backend assigns — it must survive the
    // client untouched, or the grid and the report disagree about who is who.
    expect(sent.members.map((m) => m.title)).toEqual([
      "slot 0",
      "slot 1",
      "slot 2",
    ])
    for (const m of sent.members) {
      expect(m.config.prompt_blocks).toHaveLength(1)
    }
  })

  it("never asks the backend to pin a base commit itself", async () => {
    await workTaskBatchCreate(spec(2))

    const [, params] = lastCall()
    const sent = (params as { spec: Record<string, unknown> }).spec
    // The base is resolved server-side from the folder's HEAD. A client that
    // could name it could pin a commit the repository never had — and would
    // race the very branch switch the pin exists to survive.
    expect(sent).not.toHaveProperty("base_sha")
    expect(sent).not.toHaveProperty("base_branch")
  })

  it("defaults cleanup to every member and forwards an explicit keep list", async () => {
    await workTaskBatchCleanup(3)
    expect(mocks.call).toHaveBeenLastCalledWith("work_task_batch_cleanup", {
      id: 3,
      keepTaskIds: [],
    })

    await workTaskBatchCleanup(3, [11, 12])
    expect(mocks.call).toHaveBeenLastCalledWith("work_task_batch_cleanup", {
      id: 3,
      keepTaskIds: [11, 12],
    })
  })

  /**
   * The false-success regression, at the client boundary. A cleanup that refused
   * for two of three members must arrive as three distinguishable answers — with
   * the reason a retry needs still attached.
   */
  it("returns cleanup outcomes per member, failures and reasons intact", async () => {
    const backend: BatchCleanupOutcome[] = [
      { task_id: 11, slot_index: 0, result: "succeeded" },
      {
        task_id: 12,
        slot_index: 1,
        result: "failed",
        error: "worktree is locked by another process",
      },
      { task_id: 13, slot_index: 2, result: "blocked" },
    ]
    mocks.call.mockResolvedValue(backend)

    const outcomes = await workTaskBatchCleanup(3)

    expect(outcomes).toHaveLength(3)
    expect(outcomes.filter((o) => o.result === "succeeded")).toHaveLength(1)
    // `blocked` is not `failed`: one says "stop the member first", the other
    // offers a retry. Collapsing them would mislead the user about what to do.
    expect(outcomes.find((o) => o.task_id === 13)?.result).toBe("blocked")
    expect(outcomes.find((o) => o.task_id === 12)?.error).toBe(
      "worktree is locked by another process"
    )
    // The array is NOT reducible to a success: any consumer that wants a
    // headline has to look at every entry to get one.
    expect(outcomes.every((o) => o.result === "succeeded")).toBe(false)
  })

  it("returns start outcomes per member so one refusal does not read as total failure", async () => {
    const backend: BatchMemberOutcome[] = [
      { task_id: 11, slot_index: 0, ok: true },
      { task_id: 12, slot_index: 1, ok: false, error: "task is not in todo" },
      { task_id: 13, slot_index: 2, ok: true },
    ]
    mocks.call.mockResolvedValue(backend)

    const outcomes = await workTaskBatchStart(3)

    expect(outcomes.filter((o) => o.ok)).toHaveLength(2)
    expect(outcomes.find((o) => !o.ok)?.error).toBe("task is not in todo")
  })
})
