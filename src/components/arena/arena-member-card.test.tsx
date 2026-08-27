import { render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { describe, expect, it, vi } from "vitest"

/**
 * The member card's one job that matters: never present a request as a result.
 *
 * The first cut of this card read the applied configuration from
 * `profile_snapshot` — a field written at creation time, when no agent process
 * exists and therefore no applied value does either. The "actually in effect"
 * section was silently always empty, while the card's own documentation claimed
 * it showed reality. These tests pin the corrected contract: applied values come
 * from the engine's resolved profile, a request is labelled as a request, and the
 * gaps between them are shown rather than smoothed over.
 */

import enMessages from "@/i18n/messages/en.json"
import type { WorkTaskBatchMember } from "@/lib/types"
import { ArenaMemberCard } from "./arena-member-card"

function member(over: Partial<WorkTaskBatchMember> = {}): WorkTaskBatchMember {
  return {
    id: 1,
    batch_id: 1,
    task_id: 11,
    slot_index: 0,
    task_status: "review",
    task_title: "slot",
    label: "Codex",
    conversation_id: 5,
    connection_id: null,
    files_changed: null,
    additions: null,
    deletions: null,
    ...over,
  }
}

function renderCard(over: Partial<WorkTaskBatchMember> = {}) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <ArenaMemberCard
        member={member(over)}
        onOpenTranscript={vi.fn()}
        onOpenDiff={vi.fn()}
      />
    </NextIntlClientProvider>
  )
}

describe("ArenaMemberCard — applied vs requested", () => {
  it("shows the configuration the engine actually applied", () => {
    renderCard({
      applied_profile: {
        applied: { model: "gpt-5-codex", effort: "medium" },
      },
    })
    expect(screen.getByText("gpt-5-codex")).toBeTruthy()
    expect(screen.getByText("medium")).toBeTruthy()
    // Not labelled as a request: this IS what ran.
    expect(screen.queryByText("requested")).toBeNull()
  })

  /** Before a launch there is nothing applied. Showing the request unlabelled
   *  would present an intention as an outcome. */
  it("labels the request as a request while nothing has been applied", () => {
    renderCard({
      task_status: "todo",
      profile_snapshot: {
        requested: { config_values: { model: "gpt-5-codex" } },
      },
    })
    expect(screen.getByText("requested")).toBeTruthy()
    expect(screen.getByText("gpt-5-codex")).toBeTruthy()
  })

  /** Once the engine has spoken, its answer wins — the request is no longer the
   *  interesting number. */
  it("prefers the applied configuration over the request", () => {
    renderCard({
      profile_snapshot: {
        requested: { config_values: { effort: "high" } },
      },
      applied_profile: { applied: { effort: "medium" } },
    })
    expect(screen.getByText("medium")).toBeTruthy()
    expect(screen.queryByText("requested")).toBeNull()
  })

  /** The gap is the honest part of a comparison, so it is on the card rather than
   *  behind a detail view. */
  it("shows every warning the engine recorded about the gap", () => {
    renderCard({
      applied_profile: {
        applied: { effort: "medium" },
        warnings: [
          "effort 'high' is not in the known vocabulary for Codex model gpt-5",
        ],
      },
    })
    expect(
      screen.getByText(/effort 'high' is not in the known vocabulary/)
    ).toBeTruthy()
  })

  /** Session-level policies resolve to "inherit" for every agent today; printing
   *  it on every card is noise, not information. */
  it("hides the inherit placeholders", () => {
    renderCard({
      applied_profile: {
        applied: {
          model: "gpt-5",
          "skills.mode": "inherit",
          "mcp.mode": "inherit",
        },
      },
    })
    expect(screen.getByText("gpt-5")).toBeTruthy()
    expect(screen.queryByText("inherit")).toBeNull()
  })

  /** These are opaque JSON columns and audit payloads. A shape from another build
   *  must degrade to "nothing to show" rather than throw inside a card the whole
   *  round renders through. */
  it("survives a malformed profile instead of breaking the round", () => {
    expect(() =>
      renderCard({
        applied_profile: { applied: "not-an-object" as never },
        profile_snapshot: { requested: 42 as never },
      })
    ).not.toThrow()
  })
})

describe("ArenaMemberCard — deterministic evidence", () => {
  it("reports a passing preflight with the command that ran", () => {
    renderCard({
      preflight: { status: "passed", command: "pnpm test" },
    })
    expect(screen.getByText(/Checks passed \(pnpm test\)/)).toBeTruthy()
  })

  it("reports a failing preflight", () => {
    renderCard({
      preflight: { status: "failed", command: "pnpm test", exit_code: 1 },
    })
    expect(screen.getByText(/Checks failed \(pnpm test\)/)).toBeTruthy()
  })

  it("offers the changes only once there are measured changes", () => {
    const { unmount } = renderCard({ files_changed: null })
    expect(screen.queryByRole("button", { name: "Changes" })).toBeNull()
    unmount()

    renderCard({ files_changed: 3, additions: 40, deletions: 5 })
    expect(screen.getByRole("button", { name: "Changes" })).toBeTruthy()
  })

  it("offers the transcript only once a conversation exists", () => {
    const { unmount } = renderCard({ conversation_id: null })
    expect(screen.queryByRole("button", { name: "Transcript" })).toBeNull()
    unmount()

    renderCard({ conversation_id: 5 })
    expect(screen.getByRole("button", { name: "Transcript" })).toBeTruthy()
  })
})
