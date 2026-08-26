"use client"

import type { ComponentType } from "react"
import type { LucideIcon } from "lucide-react"

/**
 * Workbench contribution registry — the seam that lets a full-page feature join
 * the workbench without editing the workbench.
 *
 * Before this existed, adding a page meant coordinated edits in five or six
 * places: a central route union, three `Record<RouteId, …>` maps, a hardcoded
 * button in the sidebar, and a tuple in the sidebar's storage module. Every new
 * page therefore touched the same high-traffic files, which is both a merge
 * magnet and an invitation to special-case the shell for one feature. Now a
 * feature declares its own route id and registers what it contributes; the shell
 * renders whatever is registered and knows nothing about any particular page.
 *
 * ## Declaring a route
 *
 * Route ids stay statically typed — an unregistered id is a compile error, not a
 * blank screen — via declaration merging from the feature's own file:
 *
 * ```ts
 * declare module "@/lib/workbench/contributions" {
 *   interface WorkbenchRoutes {
 *     myFeature: true
 *   }
 * }
 * ```
 *
 * ## What the shell still owns
 *
 * One import. Something has to pull a module in for its registration to run, and
 * a statically-exported bundle cannot discover files at runtime; that single line
 * lives in `first-party.ts`. It is the irreducible remainder — not a switch to
 * extend, a map to keep in sync, or a place where the shell learns what a page
 * is.
 */
export interface WorkbenchRoutes {
  /** The default workspace (folder / conversation tabs). Reserved: it is the
   *  surface every route is rendered *over*, so it contributes no view. */
  conversations: true
}

export type WorkbenchRouteId = keyof WorkbenchRoutes & string

/** The route that is the workspace itself rather than a page over it. */
export const CONVERSATIONS_ROUTE = "conversations" as const

/** Undo a registration. Returned by every `register*` call so a module can be
 *  disabled without the shell tracking what it added. */
export type Dispose = () => void

/** What a chrome cluster hands its route's buttons: the host's own button
 *  metrics, so one component fits both the desktop overlay (`h-6`) and the
 *  mobile title bar (`h-8`) without knowing which it is in. */
export interface WorkbenchChromeActionsProps {
  buttonClassName: string
  iconClassName: string
}

/** A full page that takes over the main content region. */
export interface WorkbenchViewContribution {
  id: WorkbenchRouteId
  /** The page itself. */
  page: ComponentType
  /** Content for the window-chrome strip above the page (usually the title). */
  strip?: ComponentType
  /** Buttons for the window's top-right chrome cluster, left of the settings
   *  gear. A full-page route hides the terminal and aux toggles (they act on the
   *  workspace it covers), so page-level controls take that space. */
  chromeActions?: ComponentType<WorkbenchChromeActionsProps>
}

/** What a nav row hands its trailing badge. The row owns the layout (the badge
 *  is pushed to the trailing edge by the class it receives); the badge owns what
 *  it says. */
export interface NavBadgeProps {
  className: string
}

/** Shared shape of a navigation registration. */
interface NavigationContributionBase {
  id: WorkbenchRouteId
  icon: LucideIcon
  /** Ascending; ties fall back to registration order. */
  order: number
  /**
   * Optional badge / hint rendered at the row's trailing edge.
   *
   * A component rather than a value on purpose: badges are live (unseen
   * failures, tasks awaiting the user) and each reads a different context. Owning
   * the subscription here keeps those hooks out of the sidebar, which would
   * otherwise have to call every feature's context to render any row.
   */
  trailing?: ComponentType<NavBadgeProps>
}

/**
 * A route the user can navigate to.
 *
 * A union rather than one shape with an optional label, because the two cases
 * genuinely differ: a sidebar row must have something to write on it, while a
 * route reached only from elsewhere in the chrome (the status-bar counter, quick
 * actions) has no row and would otherwise need a message key that nothing ever
 * renders. Expressing that here keeps it a compile error rather than a blank
 * row.
 */
export type NavigationContribution =
  | (NavigationContributionBase & {
      /** Drawn in the sidebar and offered a visibility toggle. */
      inSidebar?: true
      /** Key under the `Folder.sidebar` message namespace. */
      labelKey: string
    })
  | (NavigationContributionBase & {
      /** Registered for navigation elsewhere; takes no sidebar slot. */
      inSidebar: false
      labelKey?: string
    })

/** A navigation contribution that takes a sidebar row — and therefore has a
 *  label. What {@link listSidebarNavigationItems} returns. */
export type SidebarNavigationItem = Extract<
  NavigationContribution,
  { labelKey: string }
>

const views = new Map<WorkbenchRouteId, WorkbenchViewContribution>()
const navigation = new Map<WorkbenchRouteId, NavigationContribution>()
/** Registration order, for stable sorting when `order` ties. */
const navSequence = new Map<WorkbenchRouteId, number>()
let navCounter = 0

/**
 * Register a page.
 *
 * Re-registering an id replaces it. That is what makes hot reload survivable in
 * development, where a module file is re-evaluated without the registry being
 * torn down — the alternative (throwing on a duplicate) turns every save into a
 * hard refresh.
 */
export function registerWorkbenchView(
  contribution: WorkbenchViewContribution
): Dispose {
  if (contribution.id === CONVERSATIONS_ROUTE) {
    throw new Error(
      "the conversations route is the workspace surface and cannot be contributed as a page"
    )
  }
  views.set(contribution.id, contribution)
  return () => {
    // Only withdraw if this exact contribution is still the registered one; a
    // re-register that replaced it owns the slot now.
    if (views.get(contribution.id) === contribution)
      views.delete(contribution.id)
  }
}

export function registerNavigationItem(
  contribution: NavigationContribution
): Dispose {
  navigation.set(contribution.id, contribution)
  if (!navSequence.has(contribution.id)) {
    navSequence.set(contribution.id, navCounter++)
  }
  return () => {
    if (navigation.get(contribution.id) === contribution) {
      navigation.delete(contribution.id)
    }
  }
}

export function getWorkbenchView(
  id: WorkbenchRouteId | null | undefined
): WorkbenchViewContribution | undefined {
  return id ? views.get(id) : undefined
}

/** Every registered route id that has a page. Does NOT include
 *  `conversations`, which is the surface rather than a page. */
export function registeredRouteIds(): WorkbenchRouteId[] {
  return [...views.keys()]
}

/** True when `id` names the workspace or a registered page. The shell's guard
 *  against a route id that no longer exists — a persisted or deep-linked id
 *  whose feature was removed must fall back rather than render nothing. */
export function isKnownRoute(id: string): id is WorkbenchRouteId {
  return id === CONVERSATIONS_ROUTE || views.has(id as WorkbenchRouteId)
}

/** Navigation rows in display order. */
export function listNavigationItems(): NavigationContribution[] {
  return [...navigation.values()].sort(
    (a, b) =>
      a.order - b.order ||
      (navSequence.get(a.id) ?? 0) - (navSequence.get(b.id) ?? 0)
  )
}

/** The rows the sidebar draws and offers a visibility toggle for. Narrowed to
 *  the label-bearing variant, so a caller never has to handle a row with nothing
 *  to write on it. */
export function listSidebarNavigationItems(): SidebarNavigationItem[] {
  return listNavigationItems().filter(
    (item): item is SidebarNavigationItem => item.inSidebar !== false
  )
}

/** Reset the registry. Tests only — the production registry is populated once
 *  at import time and never cleared. */
export function resetWorkbenchContributionsForTest(): void {
  views.clear()
  navigation.clear()
  navSequence.clear()
  navCounter = 0
}
