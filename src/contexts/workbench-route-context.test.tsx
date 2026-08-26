import { fireEvent, render } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import {
  registerWorkbenchView,
  resetWorkbenchContributionsForTest,
} from "@/lib/workbench/contributions"
import {
  WorkbenchRouteProvider,
  useWorkbenchRoute,
} from "./workbench-route-context"

function Probe() {
  const { routeId, isConversations, setRoute, openConversations } =
    useWorkbenchRoute()
  return (
    <div>
      <span data-testid="route">{routeId}</span>
      <span data-testid="isConv">{String(isConversations)}</span>
      <button onClick={() => setRoute("automations")}>go</button>
      <button onClick={openConversations}>back</button>
    </div>
  )
}

beforeEach(() => {
  resetWorkbenchContributionsForTest()
  // The provider only accepts a route a page is registered for, so a test that
  // navigates has to register one. A stub page is enough — nothing renders it.
  registerWorkbenchView({ id: "automations", page: () => null })
})

afterEach(() => {
  resetWorkbenchContributionsForTest()
})

describe("WorkbenchRouteProvider", () => {
  it("defaults to the conversation workspace and switches routes", () => {
    const { getByTestId, getByText } = render(
      <WorkbenchRouteProvider>
        <Probe />
      </WorkbenchRouteProvider>
    )
    expect(getByTestId("route").textContent).toBe("conversations")
    expect(getByTestId("isConv").textContent).toBe("true")

    fireEvent.click(getByText("go"))
    expect(getByTestId("route").textContent).toBe("automations")
    expect(getByTestId("isConv").textContent).toBe("false")

    fireEvent.click(getByText("back"))
    expect(getByTestId("route").textContent).toBe("conversations")
    expect(getByTestId("isConv").textContent).toBe("true")
  })

  /**
   * A route whose page is not registered — a module that was disabled, or a
   * caller holding an id from an older build — falls back to the workspace
   * instead of leaving the content region blank with no way back.
   */
  it("falls back to the workspace for a route no page is registered for", () => {
    resetWorkbenchContributionsForTest()
    const { getByTestId, getByText } = render(
      <WorkbenchRouteProvider>
        <Probe />
      </WorkbenchRouteProvider>
    )
    fireEvent.click(getByText("go"))
    expect(getByTestId("route").textContent).toBe("conversations")
    expect(getByTestId("isConv").textContent).toBe("true")
  })

  it("throws when used outside the provider", () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {})
    expect(() => render(<Probe />)).toThrow(/WorkbenchRouteProvider/)
    spy.mockRestore()
  })
})
