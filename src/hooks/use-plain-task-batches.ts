"use client"

import { useCallback, useEffect, useRef, useState } from "react"

import { workTaskBatchList } from "@/lib/api"
import { onTransportReconnect, subscribe } from "@/lib/platform"
import type { WorkTaskBatch } from "@/lib/types"

const WORK_TASK_BATCH_CHANGED_EVENT = "task-batch://changed"
const WORK_TASK_CHANGED_EVENT = "task://changed"

/**
 * Batches that belong to no module — the ones a user made by selecting several
 * to-dos on the board.
 *
 * `owner_extension == null` is the filter, and it is the whole distinction: a
 * batch an app created is that app's to present (the Arena shows its own rounds
 * its own way), while an ownerless one is a plain bulk operation and belongs on
 * the board that started it. Core carries the field and never interprets it;
 * this is one of the two places that decides what it means.
 */
export function usePlainTaskBatches(): {
  batches: WorkTaskBatch[]
  refetch: () => Promise<void>
} {
  const [batches, setBatches] = useState<WorkTaskBatch[]>([])
  const reqRef = useRef(0)

  const refetch = useCallback(async () => {
    const id = ++reqRef.current
    try {
      const list = await workTaskBatchList(null)
      if (id !== reqRef.current) return
      setBatches(list.filter((b) => b.owner_extension == null))
    } catch {
      // Keep the previous list on a transient error; a later event recovers.
    }
  }, [])

  useEffect(() => {
    /* eslint-disable react-hooks/set-state-in-effect */
    void refetch()
    const unsubs: (() => void)[] = []
    let cancelled = false
    // Both channels: the batch row changes when its aggregate status is
    // recomputed, and a member's own transition changes what the strip shows
    // about it without the batch row moving at all.
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

  return { batches, refetch }
}
