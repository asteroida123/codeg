import { render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

vi.mock("@/lib/api", () => ({
  getDelegationPerformance: vi.fn(),
}))

import { DelegationPerformancePanel } from "./delegation-performance"
import enMessages from "@/i18n/messages/en.json"
import { getDelegationPerformance } from "@/lib/api"

const mockGet = vi.mocked(getDelegationPerformance)

function renderWithIntl() {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <DelegationPerformancePanel />
    </NextIntlClientProvider>
  )
}

/** The Rust `DelegationDimensionStats` wire shape. */
function bucket(overrides: Record<string, unknown> = {}) {
  return {
    key: "codex",
    task_count: 0,
    completed: 0,
    failed: 0,
    canceled: 0,
    unknown: 0,
    running: 0,
    success_rate: 0,
    avg_duration_ms: null,
    input_tokens: 0,
    output_tokens: 0,
    reworked: 0,
    rework_rate: 0,
    ...overrides,
  }
}

beforeEach(() => {
  mockGet.mockReset()
})

describe("DelegationPerformancePanel", () => {
  it("shows the empty hint when the ledger has no rows", async () => {
    mockGet.mockResolvedValue({
      totals: bucket({ key: null }),
      by_agent: [],
      by_model: [],
    })
    renderWithIntl()
    await waitFor(() =>
      expect(
        screen.getByText(/No delegations recorded yet/i)
      ).toBeInTheDocument()
    )
    expect(mockGet).toHaveBeenCalledTimes(1)
  })

  it("renders totals and both dimension tables with computed rates", async () => {
    mockGet.mockResolvedValue({
      totals: bucket({
        key: null,
        task_count: 3,
        completed: 2,
        failed: 1,
        running: 0,
        success_rate: 2 / 3,
        rework_rate: 1 / 3,
        avg_duration_ms: 1500,
        input_tokens: 1000,
        output_tokens: 500,
      }),
      by_agent: [bucket({ key: "codex", task_count: 3, success_rate: 2 / 3 })],
      by_model: [
        // NULL model key renders as its own "not recorded" bucket.
        bucket({ key: null, task_count: 2 }),
        bucket({ key: "gpt-6", task_count: 1 }),
      ],
    })
    renderWithIntl()

    await waitFor(() =>
      expect(screen.getByText("By agent")).toBeInTheDocument()
    )
    // Totals: 66.7% success / 33.3% rework from pre-computed rates (the
    // agent row repeats the success rate, so assert on all matches).
    expect(screen.getAllByText("66.7%").length).toBeGreaterThan(0)
    expect(screen.getAllByText("33.3%").length).toBeGreaterThan(0)
    expect(screen.getByText("1.5s")).toBeInTheDocument()
    expect(screen.getByText("1,500")).toBeInTheDocument()

    // Agent label comes from the shared agent-label helper, not the raw slug.
    expect(screen.getByText("Codex")).toBeInTheDocument()
    // Model buckets keep their raw ids; the NULL bucket is labelled.
    expect(screen.getByText("gpt-6")).toBeInTheDocument()
    expect(screen.getByText("Not recorded")).toBeInTheDocument()
  })

  it("surfaces a load failure in place", async () => {
    mockGet.mockRejectedValue(new Error("db closed"))
    renderWithIntl()
    await waitFor(() =>
      expect(
        screen.getByText(/Failed to load performance data/i)
      ).toBeInTheDocument()
    )
  })
})
