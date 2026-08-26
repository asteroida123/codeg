import { afterEach, beforeEach, describe, expect, it } from "vitest"
import type { LucideIcon } from "lucide-react"

import enMessages from "@/i18n/messages/en.json"
import {
  CONVERSATIONS_ROUTE,
  getWorkbenchView,
  isKnownRoute,
  listNavigationItems,
  listSidebarNavigationItems,
  registerNavigationItem,
  registerWorkbenchView,
  registeredRouteIds,
  resetWorkbenchContributionsForTest,
} from "@/lib/workbench/contributions"
import { activateFirstPartyModules } from "@/lib/workbench/first-party"

/**
 * The workbench contribution registry — the seam that lets a full-page feature
 * join the workbench without the workbench being edited.
 *
 * Two things are pinned here. The registry's own mechanics (register, replace,
 * dispose, ordering, the unknown-route guard), and a **parity check on the real
 * first-party registrations**: every label key must exist in the message
 * catalogue. That check earns its place because it replaces something that used
 * to be free — when nav rows were a typed tuple, next-intl verified the keys at
 * compile time. The registry deliberately does not depend on next-intl's
 * generated key union (a feature cannot be asked to satisfy it, and neither
 * could an external module), so the guarantee moves here rather than
 * disappearing.
 */

const Stub = () => null
/** Lucide icons are forwardRef components; a bare function is enough for the
 *  registry (nothing renders it here) but not for its type. */
const IconStub = Stub as unknown as LucideIcon

beforeEach(() => {
  resetWorkbenchContributionsForTest()
})

afterEach(() => {
  resetWorkbenchContributionsForTest()
})

describe("workbench contribution registry", () => {
  it("resolves a registered page, strip and chrome actions by route", () => {
    const StripStub = () => null
    const ActionsStub = () => null
    registerWorkbenchView({
      id: "tasks",
      page: Stub,
      strip: StripStub,
      chromeActions: ActionsStub,
    })

    const view = getWorkbenchView("tasks")
    expect(view?.page).toBe(Stub)
    expect(view?.strip).toBe(StripStub)
    expect(view?.chromeActions).toBe(ActionsStub)
    expect(registeredRouteIds()).toEqual(["tasks"])
  })

  it("returns nothing for an unregistered route, and for no route at all", () => {
    expect(getWorkbenchView("tasks")).toBeUndefined()
    expect(getWorkbenchView(null)).toBeUndefined()
    expect(getWorkbenchView(undefined)).toBeUndefined()
  })

  /** The workspace surface is what routes are drawn *over*; contributing it as a
   *  page would put the shell inside itself. */
  it("refuses to let the conversations route be contributed as a page", () => {
    expect(() =>
      registerWorkbenchView({ id: CONVERSATIONS_ROUTE, page: Stub })
    ).toThrow(/conversations/)
  })

  it("counts the workspace and every registered page as known", () => {
    expect(isKnownRoute(CONVERSATIONS_ROUTE)).toBe(true)
    expect(isKnownRoute("tasks")).toBe(false)
    registerWorkbenchView({ id: "tasks", page: Stub })
    expect(isKnownRoute("tasks")).toBe(true)
    expect(isKnownRoute("nothing-like-this")).toBe(false)
  })

  it("disposes a registration without disturbing the others", () => {
    const dispose = registerWorkbenchView({ id: "tasks", page: Stub })
    registerWorkbenchView({ id: "forge", page: Stub })

    dispose()

    expect(getWorkbenchView("tasks")).toBeUndefined()
    expect(getWorkbenchView("forge")).toBeTruthy()
  })

  /**
   * Re-registering replaces rather than throwing, so a module file re-evaluated
   * by hot reload converges instead of turning every save into a hard refresh.
   * The stale disposer must then be inert — otherwise the reload's own cleanup
   * would delete the registration that just replaced it.
   */
  it("replaces on re-registration, and the superseded disposer is inert", () => {
    const First = () => null
    const Second = () => null
    const disposeFirst = registerWorkbenchView({ id: "tasks", page: First })
    registerWorkbenchView({ id: "tasks", page: Second })
    expect(getWorkbenchView("tasks")?.page).toBe(Second)

    disposeFirst()

    expect(getWorkbenchView("tasks")?.page).toBe(Second)
  })

  it("orders navigation by `order`, then by registration", () => {
    registerNavigationItem({
      id: "forge",
      icon: IconStub,
      labelKey: "forge",
      order: 30,
    })
    registerNavigationItem({
      id: "tasks",
      icon: IconStub,
      labelKey: "tasks",
      order: 10,
    })
    registerNavigationItem({
      id: "automations",
      icon: IconStub,
      labelKey: "automations",
      order: 10,
    })

    // `tasks` was registered before `automations` at the same order.
    expect(listNavigationItems().map((i) => i.id)).toEqual([
      "tasks",
      "automations",
      "forge",
    ])
  })

  it("keeps an `inSidebar: false` route navigable but off the sidebar", () => {
    registerNavigationItem({
      id: "tasks",
      icon: IconStub,
      labelKey: "tasks",
      order: 10,
    })
    registerNavigationItem({
      id: "tokenUsage",
      icon: IconStub,
      labelKey: "tokenUsage",
      order: 20,
      inSidebar: false,
    })

    expect(listNavigationItems().map((i) => i.id)).toEqual([
      "tasks",
      "tokenUsage",
    ])
    expect(listSidebarNavigationItems().map((i) => i.id)).toEqual(["tasks"])
  })
})

describe("first-party registrations", () => {
  beforeEach(() => {
    activateFirstPartyModules()
  })

  it("registers a page for every route that ships", () => {
    expect(registeredRouteIds().sort()).toEqual([
      "automations",
      "forge",
      "tasks",
      "tokenUsage",
    ])
  })

  /** Replaces the compile-time check next-intl used to give the old typed tuple.
   *  A registration with a key the catalogue does not have would otherwise render
   *  the raw key as the row's label. */
  it("gives every navigation item a label key that exists in the catalogue", () => {
    const sidebarMessages = (
      enMessages as unknown as {
        Folder: { sidebar: Record<string, string> }
      }
    ).Folder.sidebar

    // Sidebar rows are the ones that render a label; the type already guarantees
    // they HAVE a key, so what is left to check is that the catalogue has it.
    const items = listSidebarNavigationItems()
    expect(items.length).toBeGreaterThan(0)
    for (const item of items) {
      expect(
        sidebarMessages[item.labelKey],
        `Folder.sidebar.${item.labelKey} is missing for the "${item.id}" route`
      ).toBeTruthy()
    }
  })

  it("puts the sidebar rows in the order users see them", () => {
    expect(listSidebarNavigationItems().map((i) => i.id)).toEqual([
      "automations",
      "tasks",
      "forge",
    ])
  })

  /** The usage report is opened from the status bar counter and quick actions;
   *  it is a report you read, not a place you work, so it takes no permanent
   *  slot above the conversation list. */
  it("keeps the usage report off the sidebar while leaving it navigable", () => {
    expect(registeredRouteIds()).toContain("tokenUsage")
    expect(listSidebarNavigationItems().map((i) => i.id)).not.toContain(
      "tokenUsage"
    )
  })

  it("is idempotent, so a re-evaluated module does not double the sidebar", () => {
    const before = listSidebarNavigationItems().map((i) => i.id)
    activateFirstPartyModules()
    expect(listSidebarNavigationItems().map((i) => i.id)).toEqual(before)
  })
})
