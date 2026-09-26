import { describe, expect, it } from "vitest"

import type { FlatFileEntry } from "@/hooks/use-file-tree"
import type {
  AcpAgentInfo,
  DbConversationSummary,
  DelegatedChildSession,
  GitLogEntry,
} from "@/lib/types"

import {
  agentToSuggestion,
  commitToSuggestion,
  compareDelegatedSessions,
  delegatedSessionToSuggestion,
  fileToSuggestion,
  sessionToSuggestion,
} from "./adapters"

describe("fileToSuggestion", () => {
  const entry: FlatFileEntry = {
    name: "app.ts",
    relativePath: "src/app.ts",
    kind: "file",
    lowerPath: "src/app.ts",
    lowerName: "app.ts",
  }
  it("maps to a file reference with a joined file:// uri", () => {
    const item = fileToSuggestion(entry, "/repo")
    expect(item.reference).toMatchObject({
      refType: "file",
      id: "src/app.ts",
      label: "app.ts",
      uri: "file:///repo/src/app.ts",
      meta: { fileKind: "file" },
    })
    expect(item.detail).toBe("src/app.ts")
  })
  it("does not double a separator when the root has a trailing slash", () => {
    expect(fileToSuggestion(entry, "/repo/").reference.uri).toBe(
      "file:///repo/src/app.ts"
    )
  })
})

describe("agentToSuggestion", () => {
  it("maps to an agent reference with a codeg://agent routing uri", () => {
    const agent = {
      agent_type: "claude_code",
      name: "Claude Code",
      description: "Anthropic CLI",
      available: true,
    } as AcpAgentInfo
    const item = agentToSuggestion(agent)
    expect(item.reference).toMatchObject({
      refType: "agent",
      id: "claude_code",
      label: "Claude Code",
      uri: "codeg://agent/claude_code",
      meta: { agentType: "claude_code", available: true },
    })
  })
})

describe("sessionToSuggestion", () => {
  const base = {
    id: 123,
    agent_type: "codex",
    status: "in_progress",
    git_branch: "main",
  } as DbConversationSummary
  it("encodes the numeric conversation id in the uri (regardless of external_id)", () => {
    const item = sessionToSuggestion({
      ...base,
      title: "Login refactor",
      external_id: "abc123",
    })
    expect(item.reference).toMatchObject({
      refType: "session",
      id: "123",
      label: "Login refactor",
      // Always the internal numeric id now — get_session_info resolves it
      // server-side via the row's bound external_id + agent_type.
      uri: "codeg://session/123",
      // meta.agentType still set, so the @-panel option row shows the agent icon.
      meta: { agentType: "codex", status: "in_progress", branch: "main" },
    })
  })
  it("uses the numeric id even when there is no external_id", () => {
    expect(sessionToSuggestion({ ...base, title: "x" }).reference.uri).toBe(
      "codeg://session/123"
    )
  })
  it("falls back to #id when the title is empty", () => {
    expect(sessionToSuggestion({ ...base, title: null }).reference.label).toBe(
      "#123"
    )
  })
  it("folds inline reference badges in the title to their label text", () => {
    // A title carrying a serialized file badge shows like the sidebar — just the
    // bracket text — in the panel row and on the inserted session badge.
    const item = sessionToSuggestion({
      ...base,
      title: "[README.md](file:///repo/README.md) fix the bug",
    })
    expect(item.reference.label).toBe("README.md fix the bug")
    expect(item.keywords).toBe("README.md fix the bug codex")
  })
  it("falls back to #id when the title is only whitespace", () => {
    expect(sessionToSuggestion({ ...base, title: "   " }).reference.label).toBe(
      "#123"
    )
  })
})

describe("commitToSuggestion", () => {
  it("maps to a commit reference with an encoded repo key", () => {
    const entry = {
      hash: "abc1234",
      full_hash: "abc1234def5678",
      author: "Jane",
      date: "2026-06-10",
      message: "fix login",
      files: [],
      pushed: true,
    } as GitLogEntry
    const item = commitToSuggestion(entry, "/repo with space")
    expect(item.reference).toMatchObject({
      refType: "commit",
      id: "abc1234def5678",
      label: "abc1234",
      uri: "codeg://commit/%2Frepo%20with%20space@abc1234def5678",
      meta: { shortHash: "abc1234", message: "fix login", pushed: true },
    })
  })
})

describe("delegatedSessionToSuggestion", () => {
  const labels = {
    round: (rounds: number) => `第 ${rounds} 轮`,
    status: (status: string) => `状态:${status}`,
  }
  const base: DelegatedChildSession = {
    parent_conversation_id: 1,
    child_conversation_id: 77,
    agent_type: "codex",
    title: "Hunt flaky test",
    git_branch: "feature/x",
    status: "running",
    continuable: false,
    rounds: 3,
    latest_task: "stabilize the login suite",
    last_activity_at: "2026-01-01T00:00:00Z",
  }

  it("inserts the same session reference shape as sessionToSuggestion", () => {
    const item = delegatedSessionToSuggestion(
      base,
      labels,
      Date.parse("2026-01-01T00:30:00Z")
    )
    expect(item.reference).toEqual({
      refType: "session",
      id: "77",
      label: "Hunt flaky test",
      uri: "codeg://session/77",
      meta: { agentType: "codex", status: "running", branch: "feature/x" },
    })
  })

  it("packs round + status + branch + relative activity into the detail", () => {
    const item = delegatedSessionToSuggestion(
      base,
      labels,
      Date.parse("2026-01-02T00:00:00Z")
    )
    expect(item.detail).toBe("第 3 轮 · 状态:running · feature/x · 1d")
  })

  it("omits empty pieces from the detail and searches the task text", () => {
    // Same-instant `now` renders the activity as "now" (still present); with
    // no branch the branch slot is simply gone from the join.
    const item = delegatedSessionToSuggestion(
      { ...base, git_branch: null },
      labels,
      Date.parse("2026-01-01T00:00:00Z")
    )
    expect(item.detail).toBe("第 3 轮 · 状态:running · now")
    expect(item.keywords).toContain("stabilize the login suite")
    expect(item.keywords).toContain("codex")
  })

  it("folds reference badges in the title and falls back to #id", () => {
    const item = delegatedSessionToSuggestion(
      { ...base, title: "[README.md](file:///repo/README.md) fix" },
      labels,
      0
    )
    expect(item.reference.label).toBe("README.md fix")
    expect(
      delegatedSessionToSuggestion({ ...base, title: "  " }, labels, 0)
        .reference.label
    ).toBe("#77")
  })
})

describe("compareDelegatedSessions (§14.4 order)", () => {
  const child = (
    over: Partial<DelegatedChildSession>
  ): DelegatedChildSession => ({
    parent_conversation_id: 1,
    child_conversation_id: 1,
    agent_type: "codex",
    title: null,
    git_branch: null,
    status: "completed",
    continuable: true,
    rounds: 1,
    latest_task: "t",
    last_activity_at: "2026-01-01T00:00:00Z",
    ...over,
  })

  it("orders running → continuable → interrupted → everything else", () => {
    const running = child({
      child_conversation_id: 1,
      status: "running",
      continuable: false,
    })
    const continuable = child({ child_conversation_id: 2 })
    const interrupted = child({
      child_conversation_id: 3,
      status: "interrupted",
      continuable: true,
    })
    const closed = child({
      child_conversation_id: 4,
      status: "canceled",
      continuable: false,
    })
    expect(compareDelegatedSessions(running, continuable)).toBeLessThan(0)
    expect(compareDelegatedSessions(continuable, interrupted)).toBeLessThan(0)
    expect(compareDelegatedSessions(interrupted, closed)).toBeLessThan(0)
  })

  it("breaks rank ties by most recent activity, then child id", () => {
    const stale = child({
      child_conversation_id: 1,
      last_activity_at: "2026-01-01T00:00:00Z",
    })
    const fresh = child({
      child_conversation_id: 2,
      last_activity_at: "2026-02-01T00:00:00Z",
    })
    expect(compareDelegatedSessions(fresh, stale)).toBeLessThan(0)

    const a = child({
      child_conversation_id: 7,
      last_activity_at: "2026-01-01T00:00:00Z",
    })
    const b = child({
      child_conversation_id: 9,
      last_activity_at: "2026-01-01T00:00:00Z",
    })
    expect(compareDelegatedSessions(b, a)).toBeLessThan(0)
  })
})

// skillToSuggestion / expertToSuggestion were retired with the `@` panel's skill
// tab — skills/commands/experts are now inserted via the `/` and `$` triggers
// (see composer/invocation-reference.ts), not adapted for the panel.
