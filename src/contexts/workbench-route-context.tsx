"use client"

import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from "react"

import {
  CONVERSATIONS_ROUTE,
  isKnownRoute,
  type WorkbenchRouteId,
} from "@/lib/workbench/contributions"

export { CONVERSATIONS_ROUTE }
export type { WorkbenchRouteId }

interface WorkbenchRouteContextValue {
  routeId: WorkbenchRouteId
  /** Convenience for the common branch — `routeId === "conversations"`. */
  isConversations: boolean
  setRoute: (id: WorkbenchRouteId) => void
  /** Sugar for returning to the conversation workspace. */
  openConversations: () => void
}

const WorkbenchRouteContext = createContext<WorkbenchRouteContextValue | null>(
  null
)

/**
 * Drives which view fills the main content region. This mirrors the codebase's
 * lifted-state idiom (search-dialog-context): the trigger lives in the sidebar
 * (which unmounts when collapsed) while the content swap is owned by
 * WorkspaceContent — both read this single source of truth.
 *
 * Which ids exist is decided by the contribution registry
 * (`@/lib/workbench/contributions`), not by a union here: a feature declares its
 * own route from its own file. `conversations` is the one reserved id — the
 * workspace surface every other route is drawn over.
 *
 * State is in-memory only: a reload lands back on the conversation workspace.
 * That is deliberate; static export rules out URL route segments, and the
 * established pattern here is in-memory context rather than query params.
 */
export function useWorkbenchRoute() {
  const ctx = useContext(WorkbenchRouteContext)
  if (!ctx) {
    throw new Error(
      "useWorkbenchRoute must be used within WorkbenchRouteProvider"
    )
  }
  return ctx
}

export function WorkbenchRouteProvider({ children }: { children: ReactNode }) {
  const [routeId, setRouteId] = useState<WorkbenchRouteId>(CONVERSATIONS_ROUTE)

  const setRoute = useCallback((id: WorkbenchRouteId) => {
    // Guard against an id no longer backed by a registered page — a module that
    // was disabled, or a caller holding an id from an older build. Falling back
    // to the workspace keeps the shell showing something real; the alternative
    // is a blank content region with no way back.
    setRouteId(isKnownRoute(id) ? id : CONVERSATIONS_ROUTE)
  }, [])
  const openConversations = useCallback(
    () => setRouteId(CONVERSATIONS_ROUTE),
    []
  )

  const value = useMemo<WorkbenchRouteContextValue>(
    () => ({
      routeId,
      isConversations: routeId === CONVERSATIONS_ROUTE,
      setRoute,
      openConversations,
    }),
    [routeId, setRoute, openConversations]
  )

  return (
    <WorkbenchRouteContext.Provider value={value}>
      {children}
    </WorkbenchRouteContext.Provider>
  )
}
