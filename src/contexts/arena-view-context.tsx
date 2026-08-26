"use client"

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react"

import { workTaskBatchList } from "@/lib/api"
import { onTransportReconnect, subscribe } from "@/lib/platform"
import type { WorkTaskBatch } from "@/lib/types"

/** Emitted when a batch row changes: created, its aggregate status recomputed
 *  from the members, canceled, or a member's cleanup outcome recorded. */
const WORK_TASK_BATCH_CHANGED_EVENT = "task-batch://changed"
/** A member's own transition. Watched too, because a member's live status, diff
 *  stats and conversation are read from the task row — a batch whose aggregate
 *  status did not change can still have a member that visibly advanced. */
const WORK_TASK_CHANGED_EVENT = "task://changed"

/** Only this module's own batches. A plain bulk operation elsewhere in the
 *  product would use the same primitive without wanting to appear here. */
const OWNER_EXTENSION = "codeg.arena"

interface ArenaViewContextValue {
  /** Comparison rounds, newest first. */
  rounds: WorkTaskBatch[]
  /** True until the first fetch settles (success OR failure), so the page can
   *  tell "still loading" from "genuinely empty" instead of flashing the empty
   *  state. Never flips back on later refetches. */
  loading: boolean
  refetch: () => Promise<void>
}

const ArenaViewContext = createContext<ArenaViewContextValue | null>(null)

export function useArenaView() {
  const ctx = useContext(ArenaViewContext)
  if (!ctx) {
    throw new Error("useArenaView must be used within ArenaViewProvider")
  }
  return ctx
}

/**
 * Data layer for the Arena: the list of comparison rounds, plus a realtime
 * subscription.
 *
 * Read-only on purpose. Every mutation goes through a backend command
 * (`workTaskBatchStart` / `Cancel` / `Cleanup`) and comes back as an event; this
 * provider holds no orchestration state and makes no decision the backend has
 * not already made. That is the difference from the implementation this one
 * replaces, whose 1400-line front-end hook *was* the state machine — so closing
 * the window, or opening a second one, changed what the round did. Here a round
 * runs in the task engine and any number of clients watch it.
 */
export function ArenaViewProvider({ children }: { children: ReactNode }) {
  const [rounds, setRounds] = useState<WorkTaskBatch[]>([])
  const [loading, setLoading] = useState(true)
  const reqRef = useRef(0)

  const refetch = useCallback(async () => {
    const id = ++reqRef.current
    try {
      const list = await workTaskBatchList(null)
      // Drop a stale response, and keep the previous list on a transient error
      // rather than blanking the page (the tasks/automations idiom).
      if (id !== reqRef.current) return
      setRounds(list.filter((b) => b.owner_extension === OWNER_EXTENSION))
      setLoading(false)
    } catch {
      if (id === reqRef.current) setLoading(false)
    }
  }, [])

  useEffect(() => {
    /* eslint-disable react-hooks/set-state-in-effect */
    void refetch()
    const unsubs: (() => void)[] = []
    let cancelled = false
    for (const channel of [
      WORK_TASK_BATCH_CHANGED_EVENT,
      WORK_TASK_CHANGED_EVENT,
    ]) {
      void subscribe(channel, () => {
        void refetch()
      }).then((u: () => void) => {
        if (cancelled) u()
        else unsubs.push(u)
      })
    }
    // Events fired while the socket was down are dropped by the broadcaster, so
    // a round that finished during the gap would otherwise leave the page stale.
    // No-op on desktop IPC.
    const offReconnect = onTransportReconnect(() => {
      void refetch()
    })
    return () => {
      cancelled = true
      for (const u of unsubs) u()
      offReconnect?.()
    }
    /* eslint-enable react-hooks/set-state-in-effect */
  }, [refetch])

  const value = useMemo<ArenaViewContextValue>(
    () => ({ rounds, loading, refetch }),
    [rounds, loading, refetch]
  )

  return (
    <ArenaViewContext.Provider value={value}>
      {children}
    </ArenaViewContext.Provider>
  )
}

export { OWNER_EXTENSION as ARENA_OWNER_EXTENSION }
