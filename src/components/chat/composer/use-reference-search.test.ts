import { act, renderHook } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import type { FlatFileEntry } from "@/hooks/use-file-tree"
import type {
  AcpAgentInfo,
  DbConversationSummary,
  DelegatedChildSession,
  GitLogEntry,
} from "@/lib/types"

import type { SuggestionGroup, SuggestionGroupKind } from "./suggestion/types"
import {
  buildReferenceGroups,
  DEFAULT_GROUP_LABELS,
  useReferenceSearch,
  type ReferenceGroupLabels,
  type ReferenceSearchSources,
} from "./use-reference-search"

// --- fixtures ---------------------------------------------------------------

function makeFile(
  relativePath: string,
  kind: "file" | "dir" = "file"
): FlatFileEntry {
  const name = relativePath.split("/").pop() ?? relativePath
  return {
    name,
    relativePath,
    kind,
    lowerPath: relativePath.toLowerCase(),
    lowerName: name.toLowerCase(),
  }
}

function makeAgent(
  agentType: string,
  over: { name?: string; description?: string; enabled?: boolean } = {}
): AcpAgentInfo {
  return {
    agent_type: agentType,
    name: over.name ?? agentType,
    description: over.description ?? "",
    available: true,
    enabled: over.enabled ?? true,
    sort_order: 0,
  } as unknown as AcpAgentInfo
}

function makeConversation(id: number, title: string): DbConversationSummary {
  return {
    id,
    title,
    agent_type: "claude_code",
    status: "idle",
    git_branch: null,
  } as unknown as DbConversationSummary
}

function makeDelegatedChild(
  id: number,
  over: Partial<DelegatedChildSession> = {}
): DelegatedChildSession {
  return {
    parent_conversation_id: 1,
    child_conversation_id: id,
    agent_type: "codex",
    title: `Child ${id}`,
    git_branch: null,
    status: "completed",
    continuable: true,
    rounds: 1,
    latest_task: "do the thing",
    last_activity_at: "2026-01-01T00:00:00Z",
    ...over,
  }
}

/** Localized labels with distinctive text, exercising the injected pieces. */
const DELEGATED_LABELS: ReferenceGroupLabels = {
  ...DEFAULT_GROUP_LABELS,
  delegatedSession: "本次对话的子智能体",
  delegationRound: (rounds) => `第 ${rounds} 轮`,
  delegationStatus: (status) => `状态:${status}`,
}

function makeCommit(
  hash: string,
  message = "msg",
  author = "Dev"
): GitLogEntry {
  return {
    hash,
    full_hash: `${hash}0000`,
    author,
    date: "2026-01-01",
    message,
    files: [],
    pushed: false,
  }
}

function emptySources(
  over: Partial<ReferenceSearchSources> = {}
): ReferenceSearchSources {
  return {
    files: [],
    workspaceRoot: null,
    agents: [],
    sessions: [],
    commits: [],
    repoKey: null,
    ...over,
  }
}

const itemsOf = (groups: SuggestionGroup[], kind: SuggestionGroupKind) =>
  groups.find((g) => g.kind === kind)?.items ?? []

// --- pure builder -----------------------------------------------------------

describe("buildReferenceGroups", () => {
  it("returns the four groups in a fixed order (no skill group)", () => {
    const groups = buildReferenceGroups("", emptySources())
    expect(groups.map((g) => g.kind)).toEqual([
      "file",
      "agent",
      "session",
      "commit",
    ])
  })

  it("keeps every group present (empty groups are not dropped)", () => {
    const groups = buildReferenceGroups("", emptySources())
    expect(groups).toHaveLength(4)
    expect(groups.every((g) => g.items.length === 0)).toBe(true)
  })

  it("defaults the group headings to the English labels", () => {
    const groups = buildReferenceGroups("", emptySources())
    expect(groups.map((g) => g.label)).toEqual([
      DEFAULT_GROUP_LABELS.file,
      DEFAULT_GROUP_LABELS.agent,
      DEFAULT_GROUP_LABELS.session,
      DEFAULT_GROUP_LABELS.commit,
    ])
  })

  it("accepts injected (localized) group headings", () => {
    const labels = {
      ...DEFAULT_GROUP_LABELS,
      file: "文件",
      agent: "智能体",
      session: "会话",
      commit: "提交",
      skill: "技能",
    }
    const groups = buildReferenceGroups("", emptySources(), labels)
    expect(itemsOf(groups, "file")).toBeDefined()
    expect(groups.find((g) => g.kind === "agent")?.label).toBe("智能体")
  })

  it("adapts files into file:// references rooted at the workspace", () => {
    const groups = buildReferenceGroups(
      "",
      emptySources({
        files: [makeFile("a.ts"), makeFile("src/app.ts")],
        workspaceRoot: "/repo",
      })
    )
    const files = itemsOf(groups, "file")
    expect(files).toHaveLength(2)
    expect(files.map((f) => f.reference.uri)).toEqual([
      "file:///repo/a.ts",
      "file:///repo/src/app.ts",
    ])
  })

  it("filters files by name or relative path, case-insensitively", () => {
    const groups = buildReferenceGroups(
      "APP",
      emptySources({
        files: [makeFile("a.ts"), makeFile("src/App.tsx")],
        workspaceRoot: "/repo",
      })
    )
    const files = itemsOf(groups, "file")
    expect(files).toHaveLength(1)
    expect(files[0].reference.id).toBe("src/App.tsx")
  })

  it("omits the file group when there is no workspace root (R8)", () => {
    const groups = buildReferenceGroups(
      "",
      emptySources({ files: [makeFile("a.ts")], workspaceRoot: null })
    )
    expect(itemsOf(groups, "file")).toHaveLength(0)
  })

  it("filters agents by name / type / description", () => {
    const groups = buildReferenceGroups(
      "codex",
      emptySources({
        agents: [
          makeAgent("codex", { name: "Codex" }),
          makeAgent("gemini", { name: "Gemini" }),
        ],
      })
    )
    const agents = itemsOf(groups, "agent")
    expect(agents).toHaveLength(1)
    expect(agents[0].reference.id).toBe("codex")
  })

  it("excludes disabled agents (only enabled agents are mentionable)", () => {
    const groups = buildReferenceGroups(
      "",
      emptySources({
        agents: [
          makeAgent("codex", { name: "Codex" }),
          makeAgent("gemini", { name: "Gemini", enabled: false }),
        ],
      })
    )
    const agents = itemsOf(groups, "agent")
    expect(agents).toHaveLength(1)
    expect(agents[0].reference.id).toBe("codex")
  })

  it("adapts sessions into codeg://session references", () => {
    const groups = buildReferenceGroups(
      "login",
      emptySources({
        sessions: [
          makeConversation(7, "Login refactor"),
          makeConversation(8, "Sidebar perf"),
        ],
      })
    )
    const sessions = itemsOf(groups, "session")
    expect(sessions).toHaveLength(1)
    expect(sessions[0].reference.uri).toBe("codeg://session/7")
  })

  // --- delegated children (§14.4) -------------------------------------------

  it("omits the delegated-children group when the conversation has none", () => {
    const groups = buildReferenceGroups("", emptySources())
    expect(groups.map((g) => g.kind)).toEqual([
      "file",
      "agent",
      "session",
      "commit",
    ])
  })

  it("places the delegated-children group between agents and sessions", () => {
    const groups = buildReferenceGroups(
      "",
      emptySources({
        delegatedSessions: [makeDelegatedChild(3)],
        sessions: [makeConversation(7, "Some session")],
      })
    )
    expect(groups.map((g) => g.kind)).toEqual([
      "file",
      "agent",
      "delegatedSession",
      "session",
      "commit",
    ])
    expect(groups.find((g) => g.kind === "delegatedSession")?.label).toBe(
      DEFAULT_GROUP_LABELS.delegatedSession
    )
  })

  it("inserts delegated children as session references with round/status detail", () => {
    const groups = buildReferenceGroups(
      "",
      emptySources({
        delegatedSessions: [
          makeDelegatedChild(5, {
            status: "running",
            continuable: false,
            rounds: 3,
            git_branch: "feature/x",
            latest_task: "hunt the flaky test",
          }),
        ],
      }),
      DELEGATED_LABELS,
      Date.parse("2026-01-02T00:00:00Z")
    )
    const items = itemsOf(groups, "delegatedSession")
    expect(items).toHaveLength(1)
    expect(items[0].reference).toEqual({
      refType: "session",
      id: "5",
      label: "Child 5",
      uri: "codeg://session/5",
      meta: { agentType: "codex", status: "running", branch: "feature/x" },
    })
    // Round + status (localized) + branch + relative activity in the detail.
    expect(items[0].detail).toContain("第 3 轮")
    expect(items[0].detail).toContain("状态:running")
    expect(items[0].detail).toContain("feature/x")
    expect(items[0].detail).toContain("1d")
    // The task text is searchable…
    expect(items[0].keywords).toContain("hunt the flaky test")
  })

  it("orders delegated children running → continuable → needs recovery → closed", () => {
    const groups = buildReferenceGroups(
      "",
      emptySources({
        delegatedSessions: [
          // Deliberately listed out of order: closed, recovery, stale
          // continuable, fresh continuable, running.
          makeDelegatedChild(1, {
            status: "canceled",
            continuable: false,
            last_activity_at: "2026-03-01T00:00:00Z",
          }),
          makeDelegatedChild(2, {
            status: "interrupted",
            continuable: true,
            last_activity_at: "2026-03-01T00:00:00Z",
          }),
          makeDelegatedChild(3, {
            status: "completed",
            continuable: true,
            last_activity_at: "2026-01-01T00:00:00Z",
          }),
          makeDelegatedChild(4, {
            status: "completed",
            continuable: true,
            last_activity_at: "2026-02-01T00:00:00Z",
          }),
          makeDelegatedChild(5, {
            status: "running",
            continuable: false,
            last_activity_at: "2026-01-01T00:00:00Z",
          }),
        ],
      })
    )
    expect(
      itemsOf(groups, "delegatedSession").map((i) => i.reference.id)
    ).toEqual(["5", "4", "3", "2", "1"])
  })

  it("falls back to #id for an untitled delegated child", () => {
    const groups = buildReferenceGroups(
      "",
      emptySources({
        delegatedSessions: [makeDelegatedChild(9, { title: "   " })],
      })
    )
    expect(itemsOf(groups, "delegatedSession")[0].reference.label).toBe("#9")
  })

  it("filters delegated children by title, task text, agent, and branch", () => {
    const sources = emptySources({
      delegatedSessions: [
        makeDelegatedChild(1, { latest_task: "hunt the flaky test" }),
        makeDelegatedChild(2, {
          agent_type: "claude_code",
          latest_task: "unrelated",
          git_branch: "feature/y",
        }),
      ],
    })
    expect(
      itemsOf(buildReferenceGroups("flaky", sources), "delegatedSession")
    ).toHaveLength(1)
    expect(
      itemsOf(buildReferenceGroups("feature/y", sources), "delegatedSession")
    ).toHaveLength(1)
    expect(
      itemsOf(buildReferenceGroups("claude_code", sources), "delegatedSession")
    ).toHaveLength(1)
    expect(
      itemsOf(buildReferenceGroups("nomatch", sources), "delegatedSession")
    ).toHaveLength(0)
  })

  it("omits the commit group when there is no repoKey (R8)", () => {
    const groups = buildReferenceGroups(
      "",
      emptySources({ commits: [makeCommit("abc1234")], repoKey: null })
    )
    expect(itemsOf(groups, "commit")).toHaveLength(0)
  })

  it("adapts commits and filters by hash / message / author", () => {
    const groups = buildReferenceGroups(
      "bugfix",
      emptySources({
        commits: [
          makeCommit("abc1234", "bugfix: crash"),
          makeCommit("def5678", "feature"),
        ],
        repoKey: "/repo",
      })
    )
    const commits = itemsOf(groups, "commit")
    expect(commits).toHaveLength(1)
    expect(commits[0].reference.uri).toBe("codeg://commit/%2Frepo@abc12340000")
  })

  it("caps each group at 50 items and flags the overflow as truncated", () => {
    const files = Array.from({ length: 60 }, (_, i) => makeFile(`f${i}.ts`))
    const groups = buildReferenceGroups(
      "",
      emptySources({ files, workspaceRoot: "/repo" })
    )
    const fileGroup = groups.find((g) => g.kind === "file")
    expect(fileGroup?.items).toHaveLength(50)
    expect(fileGroup?.truncated).toBe(true)
  })

  it("does not flag truncation when a group exactly fills the cap", () => {
    const files = Array.from({ length: 50 }, (_, i) => makeFile(`f${i}.ts`))
    const groups = buildReferenceGroups(
      "",
      emptySources({ files, workspaceRoot: "/repo" })
    )
    const fileGroup = groups.find((g) => g.kind === "file")
    expect(fileGroup?.items).toHaveLength(50)
    expect(fileGroup?.truncated).toBe(false)
  })

  it("flags truncation for slice-based groups (agents) as well", () => {
    const agents = Array.from({ length: 51 }, (_, i) => makeAgent(`a${i}`))
    const groups = buildReferenceGroups("", emptySources({ agents }))
    const agentGroup = groups.find((g) => g.kind === "agent")
    expect(agentGroup?.items).toHaveLength(50)
    expect(agentGroup?.truncated).toBe(true)
  })

  it("returns everything for an empty query (whitespace-trimmed)", () => {
    const groups = buildReferenceGroups(
      "   ",
      emptySources({
        agents: [makeAgent("codex"), makeAgent("gemini")],
      })
    )
    expect(itemsOf(groups, "agent")).toHaveLength(2)
  })
})

// --- hook --------------------------------------------------------------------

const mocks = vi.hoisted(() => ({
  agents: [] as AcpAgentInfo[],
  files: { allFiles: [] as FlatFileEntry[], loaded: false },
  listAllConversations: vi.fn(),
  gitLog: vi.fn(),
  listDelegatedChildSessions: vi.fn(),
}))

vi.mock("@/hooks/use-file-tree", () => ({
  useFileTree: () => ({
    allFiles: mocks.files.allFiles,
    loaded: mocks.files.loaded,
    loading: false,
    reset: () => {},
  }),
}))
vi.mock("@/hooks/use-acp-agents", () => ({
  useAcpAgents: () => ({ agents: mocks.agents, fresh: true, refresh: vi.fn() }),
}))
vi.mock("@/lib/api", () => ({
  listAllConversations: (...args: unknown[]) =>
    mocks.listAllConversations(...args),
  gitLog: (...args: unknown[]) => mocks.gitLog(...args),
  listDelegatedChildSessions: (...args: unknown[]) =>
    mocks.listDelegatedChildSessions(...args),
}))

describe("useReferenceSearch", () => {
  beforeEach(() => {
    mocks.agents = []
    mocks.files = { allFiles: [], loaded: false }
    mocks.listAllConversations.mockReset().mockResolvedValue([])
    mocks.gitLog
      .mockReset()
      .mockResolvedValue({ entries: [], has_upstream: false })
    mocks.listDelegatedChildSessions.mockReset().mockResolvedValue([])
  })

  it("returns a referentially stable search across data-source updates (R7)", async () => {
    mocks.agents = [makeAgent("codex", { name: "Codex" })]
    const { result, rerender } = renderHook(
      (props: { enabled: boolean }) => useReferenceSearch(props),
      { initialProps: { enabled: true } }
    )
    const first = result.current

    // A background refresh swaps the agents array reference.
    mocks.agents = [
      makeAgent("codex", { name: "Codex" }),
      makeAgent("gemini", { name: "Gemini" }),
    ]
    rerender({ enabled: true })

    // Identity is unchanged — the popup will not re-fetch or reset selection…
    expect(result.current).toBe(first)
    // …yet the stable function reads the freshest data through its refs.
    let groups!: SuggestionGroup[]
    await act(async () => {
      groups = (await result.current("")) as SuggestionGroup[]
    })
    expect(itemsOf(groups, "agent")).toHaveLength(2)
  })

  it("lazily fetches and awaits sessions + commits on the first search", async () => {
    mocks.listAllConversations.mockResolvedValue([
      makeConversation(7, "Login refactor"),
    ])
    mocks.gitLog.mockResolvedValue({
      entries: [makeCommit("abc1234", "fix")],
      has_upstream: false,
    })
    mocks.files = { allFiles: [makeFile("a.ts")], loaded: true }

    const { result } = renderHook(() =>
      useReferenceSearch({ defaultPath: "/repo", enabled: true })
    )
    let groups!: SuggestionGroup[]
    await act(async () => {
      groups = (await result.current("")) as SuggestionGroup[]
    })

    expect(mocks.listAllConversations).toHaveBeenCalledTimes(1)
    expect(mocks.gitLog).toHaveBeenCalledWith("/repo", 100)
    expect(itemsOf(groups, "session")).toHaveLength(1)
    expect(itemsOf(groups, "commit")).toHaveLength(1)
    expect(itemsOf(groups, "file")).toHaveLength(1)
  })

  it("reuses the cached network promises across repeated searches", async () => {
    const { result } = renderHook(() =>
      useReferenceSearch({ defaultPath: "/repo", enabled: true })
    )
    await act(async () => {
      await result.current("")
      await result.current("a")
      await result.current("ab")
    })
    expect(mocks.listAllConversations).toHaveBeenCalledTimes(1)
    expect(mocks.gitLog).toHaveBeenCalledTimes(1)
  })

  it("resolves to no groups and touches no network when disabled", async () => {
    const { result } = renderHook(() =>
      useReferenceSearch({ defaultPath: "/repo", enabled: false })
    )
    let groups!: SuggestionGroup[]
    await act(async () => {
      groups = (await result.current("x")) as SuggestionGroup[]
    })
    expect(groups).toEqual([])
    expect(mocks.listAllConversations).not.toHaveBeenCalled()
    expect(mocks.gitLog).not.toHaveBeenCalled()
  })

  it("returns no groups when the query is aborted mid-fetch", async () => {
    mocks.listAllConversations.mockImplementation(
      () =>
        new Promise((resolve) =>
          setTimeout(() => resolve([makeConversation(1, "x")]), 10)
        )
    )
    const { result } = renderHook(() =>
      useReferenceSearch({ defaultPath: "/repo", enabled: true })
    )
    let groups!: SuggestionGroup[]
    await act(async () => {
      const controller = new AbortController()
      const pending = result.current("", controller.signal)
      controller.abort()
      groups = (await pending) as SuggestionGroup[]
    })
    expect(groups).toEqual([])
  })

  it("degrades gracefully with no workspace path: agents resolve, files/commits stay empty (R8)", async () => {
    mocks.agents = [makeAgent("codex", { name: "Codex" })]
    mocks.files = { allFiles: [makeFile("a.ts")], loaded: true }

    const { result } = renderHook(() => useReferenceSearch({ enabled: true }))
    let groups!: SuggestionGroup[]
    await act(async () => {
      groups = (await result.current("")) as SuggestionGroup[]
    })

    expect(itemsOf(groups, "file")).toHaveLength(0)
    expect(itemsOf(groups, "commit")).toHaveLength(0)
    expect(itemsOf(groups, "agent")).toHaveLength(1)
    expect(mocks.gitLog).not.toHaveBeenCalled()
  })

  it("does not leak the previous folder's commits when defaultPath changes mid-fetch", async () => {
    // git-log for repo A hangs until we resolve it by hand; repo B resolves
    // immediately. We switch folders before A resolves.
    let resolveA!: (value: {
      entries: GitLogEntry[]
      has_upstream: boolean
    }) => void
    mocks.gitLog.mockImplementation((repoPath: string) => {
      if (repoPath === "/repoA") {
        return new Promise((resolve) => {
          resolveA = resolve
        })
      }
      return Promise.resolve({
        entries: [makeCommit("bbb", "repo B commit")],
        has_upstream: false,
      })
    })

    const { result, rerender } = renderHook(
      (props: { defaultPath: string }) =>
        useReferenceSearch({ defaultPath: props.defaultPath, enabled: true }),
      { initialProps: { defaultPath: "/repoA" } }
    )

    // Start a search that hangs on gitLog("/repoA").
    let pending!: SuggestionGroup[] | Promise<SuggestionGroup[]>
    await act(async () => {
      pending = result.current("")
    })

    // The composer switches to repo B; commit flushes the ref mirror (pathRef →
    // "/repoB") before the stale repo A fetch resolves.
    //
    // NOTE: jsdom + RTL `act()` flush BOTH layout and passive effects at their
    // boundaries, so a unit test cannot reproduce the real-browser window where
    // a passive effect lags behind a macrotask that resolves the stale promise.
    // This asserts the guard CONTRACT (a folder switch discards the stale
    // result); the production fix that closes the timing window is the
    // commit-synchronous layout-effect mirror of pathRef in the hook.
    await act(async () => {
      rerender({ defaultPath: "/repoB" })
    })

    let groups!: SuggestionGroup[]
    await act(async () => {
      resolveA({
        entries: [makeCommit("aaa", "repo A commit")],
        has_upstream: false,
      })
      groups = (await pending) as SuggestionGroup[]
    })

    // The stale invocation must not render repo A commits into repo B's panel;
    // it bails so the next keystroke re-queries the current folder.
    expect(groups).toEqual([])
  })

  it("retries a lazy fetch after it rejects (a failure is never cached)", async () => {
    mocks.listAllConversations
      .mockRejectedValueOnce(new Error("transient"))
      .mockResolvedValueOnce([makeConversation(1, "Recovered")])

    const { result } = renderHook(() => useReferenceSearch({ enabled: true }))

    let first!: SuggestionGroup[]
    await act(async () => {
      first = (await result.current("")) as SuggestionGroup[]
    })
    // First fetch rejected → the session group is empty, but the others render.
    expect(itemsOf(first, "session")).toHaveLength(0)

    let second!: SuggestionGroup[]
    await act(async () => {
      second = (await result.current("")) as SuggestionGroup[]
    })
    // The failure was not cached, so the second `@` issues a fresh request…
    expect(mocks.listAllConversations).toHaveBeenCalledTimes(2)
    // …which succeeds and populates the group.
    expect(itemsOf(second, "session")).toHaveLength(1)
  })

  // --- delegated children (§14.4) -------------------------------------------

  it("lazily fetches the conversation's delegated children on the first search", async () => {
    mocks.listDelegatedChildSessions.mockResolvedValue([
      makeDelegatedChild(5, { status: "running" }),
    ])

    const { result } = renderHook(() =>
      useReferenceSearch({ enabled: true, conversationId: 42 })
    )
    let groups!: SuggestionGroup[]
    await act(async () => {
      groups = (await result.current("")) as SuggestionGroup[]
    })

    expect(mocks.listDelegatedChildSessions).toHaveBeenCalledWith(42)
    expect(groups.map((g) => g.kind)).toContain("delegatedSession")
    expect(itemsOf(groups, "delegatedSession")).toHaveLength(1)
    expect(itemsOf(groups, "delegatedSession")[0].reference.uri).toBe(
      "codeg://session/5"
    )

    // Cached across searches — one fetch per conversation id.
    await act(async () => {
      await result.current("chi")
    })
    expect(mocks.listDelegatedChildSessions).toHaveBeenCalledTimes(1)
  })

  it("never fetches delegated children without a conversationId", async () => {
    const { result } = renderHook(() => useReferenceSearch({ enabled: true }))
    let groups!: SuggestionGroup[]
    await act(async () => {
      groups = (await result.current("")) as SuggestionGroup[]
    })
    expect(mocks.listDelegatedChildSessions).not.toHaveBeenCalled()
    expect(groups.map((g) => g.kind)).not.toContain("delegatedSession")
  })

  it("keeps the delegated group hidden when the conversation has no children", async () => {
    mocks.listDelegatedChildSessions.mockResolvedValue([])
    const { result } = renderHook(() =>
      useReferenceSearch({ enabled: true, conversationId: 42 })
    )
    let groups!: SuggestionGroup[]
    await act(async () => {
      groups = (await result.current("")) as SuggestionGroup[]
    })
    expect(mocks.listDelegatedChildSessions).toHaveBeenCalledWith(42)
    expect(groups.map((g) => g.kind)).toEqual([
      "file",
      "agent",
      "session",
      "commit",
    ])
  })

  it("refetches when the conversation changes and discards the stale result mid-flight", async () => {
    // Conversation A's fetch hangs until released by hand; B resolves at once.
    let resolveA!: (value: DelegatedChildSession[]) => void
    mocks.listDelegatedChildSessions.mockImplementation((id: number) => {
      if (id === 1) {
        return new Promise((resolve) => {
          resolveA = resolve
        })
      }
      return Promise.resolve([makeDelegatedChild(9)])
    })

    const { result, rerender } = renderHook(
      (props: { conversationId: number | null }) =>
        useReferenceSearch({
          enabled: true,
          conversationId: props.conversationId,
        }),
      { initialProps: { conversationId: 1 } }
    )

    let pending!: SuggestionGroup[] | Promise<SuggestionGroup[]>
    await act(async () => {
      pending = result.current("")
    })

    // The composer switches to conversation 2 before 1's fetch resolves.
    await act(async () => {
      rerender({ conversationId: 2 })
    })

    let groups!: SuggestionGroup[]
    await act(async () => {
      resolveA([makeDelegatedChild(1)])
      groups = (await pending) as SuggestionGroup[]
    })
    // The stale invocation bails; conversation 1's children never leak into
    // conversation 2's panel.
    expect(groups).toEqual([])

    let fresh!: SuggestionGroup[]
    await act(async () => {
      fresh = (await result.current("")) as SuggestionGroup[]
    })
    expect(mocks.listDelegatedChildSessions).toHaveBeenCalledWith(2)
    expect(itemsOf(fresh, "delegatedSession")[0].reference.id).toBe("9")
  })
})
