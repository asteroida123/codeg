"use client"

import { useWorkbenchRoute } from "@/contexts/workbench-route-context"
import {
  getWorkbenchView,
  type WorkbenchChromeActionsProps,
} from "@/lib/workbench/contributions"

export type { WorkbenchChromeActionsProps }

/**
 * Renders whatever page is registered for the active route.
 *
 * There is no list of pages here any more, and that is the point: this file used
 * to hold three `Record<WorkbenchRouteId, ComponentType>` maps that every new
 * feature had to be threaded into. Pages now register themselves
 * (`@/lib/workbench/contributions`), so the shell resolves one lookup and knows
 * nothing about any particular page.
 *
 * WorkspaceContent overlays this on top of the (kept-mounted, hidden)
 * conversation surface so live sessions survive the swap.
 */
export function WorkbenchRoutePage() {
  const { routeId } = useWorkbenchRoute()
  const Page = getWorkbenchView(routeId)?.page
  return Page ? <Page /> : null
}

/** The active route's strip content (page title), or nothing. */
export function WorkbenchRouteStrip() {
  const { routeId } = useWorkbenchRoute()
  const Strip = getWorkbenchView(routeId)?.strip
  return Strip ? <Strip /> : null
}

/** The active route's chrome-cluster buttons, or nothing. Rendered by both
 *  chrome hosts (RightEdgeChrome on desktop, FolderTitleBar on mobile). */
export function WorkbenchRouteChromeActions(
  props: WorkbenchChromeActionsProps
) {
  const { routeId } = useWorkbenchRoute()
  const Actions = getWorkbenchView(routeId)?.chromeActions
  return Actions ? <Actions {...props} /> : null
}

/** Whether the active route contributes chrome-strip content — lets the host
 *  style the band (e.g. its bottom border) only when a title renders. */
export function useHasWorkbenchRouteStrip(): boolean {
  const { routeId } = useWorkbenchRoute()
  return getWorkbenchView(routeId)?.strip != null
}
