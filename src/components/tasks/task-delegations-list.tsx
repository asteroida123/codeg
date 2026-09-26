"use client"

import { useCallback, useEffect, useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { Loader2, MessageSquareText } from "lucide-react"
import { getFolderConversation, taskDelegations } from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import { onTransportReconnect, subscribe } from "@/lib/platform"
import { AgentIcon } from "@/components/agent-icon"
import { cn } from "@/lib/utils"
import type { AgentType, WorkTask, WorkTaskDelegation } from "@/lib/types"

const WORK_TASK_CHANGED_EVENT = "task://changed"

interface TaskDelegationsListProps {
  /** The host drawer's open state: the section stops reading while it is
   *  closed (the content stays mounted after its first open). */
  open: boolean
  task: WorkTask
}

/**
 * The sub-agent runs admitted while this task executed — ledger rows linked by
 * `delegation_task.work_task_id`. Attribution is STORED at admission, not
 * derived from the task's current conversation, so a later fresh-session
 * rework cannot repoint history.
 *
 * A row whose `source_task_id` is set is a continuation round of that earlier
 * run: the ledger's own chain, shown as the round count (a child can execute
 * several rounds, each reserving its own row).
 */
export function TaskDelegationsList({ open, task }: TaskDelegationsListProps) {
  const t = useTranslations("Tasks")
  const [delegations, setDelegations] = useState<WorkTaskDelegation[]>([])
  const [opening, setOpening] = useState<number | null>(null)
  const taskId = task.id
  // Delegations come and go while a run is live; the parent's own transitions
  // are the moments a new one can have been admitted.
  const reloadKey = `${task.run_seq}:${task.status}`

  const reload = useCallback(async () => {
    try {
      setDelegations(await taskDelegations(taskId))
    } catch {
      // Keep the previous list on a transient read failure.
    }
  }, [taskId])

  useEffect(() => {
    if (!open) return
    let unsub: (() => void) | undefined
    let cancelled = false
    void subscribe(WORK_TASK_CHANGED_EVENT, () => {
      void reload()
    }).then((u: () => void) => {
      if (cancelled) u()
      else unsub = u
    })
    const offReconnect = onTransportReconnect(() => {
      void reload()
    })
    void reload()
    return () => {
      cancelled = true
      unsub?.()
      offReconnect?.()
    }
  }, [open, reload, reloadKey])

  /** Open the child's own session as a workspace tab — its folder is the one
   *  the child executed in, which the conversation row records. */
  const openSession = useCallback(async (childConversationId: number) => {
    setOpening(childConversationId)
    try {
      const detail = await getFolderConversation(childConversationId)
      // Imported on demand: the tab store reads the workspace store at module
      // scope, and a static import would drag that whole graph into every
      // surface that mounts the task detail.
      const { useTabStore } = await import("@/stores/tab-store")
      useTabStore
        .getState()
        .openTab(
          detail.summary.folder_id,
          childConversationId,
          detail.summary.agent_type,
          false,
          detail.summary.title ?? undefined
        )
    } catch (e) {
      toast.error(toErrorMessage(e))
    } finally {
      setOpening(null)
    }
  }, [])

  return (
    <section className="flex flex-col gap-1.5">
      <h3 className="text-[0.6875rem] font-medium uppercase tracking-wide text-muted-foreground">
        {t("detailDelegations")}
      </h3>
      {delegations.length === 0 ? (
        <p className="text-xs text-muted-foreground">
          {t("detailDelegationsEmpty")}
        </p>
      ) : (
        <ul className="flex flex-col divide-y divide-border/60 overflow-hidden rounded-xl border border-border">
          {delegations.map((d) => (
            <li key={d.task_id} className="flex flex-col gap-1 px-2.5 py-2">
              <div className="flex min-w-0 items-center gap-2 text-xs">
                <span className="flex min-w-0 flex-1 items-center gap-1.5">
                  {d.agent_type ? (
                    <AgentIcon
                      agentType={d.agent_type as AgentType}
                      className="size-3.5"
                    />
                  ) : null}
                  <span className="min-w-0 truncate">{firstLine(d.task)}</span>
                </span>
                <span
                  className={cn(
                    "shrink-0 text-[0.6875rem]",
                    d.status === "failed" && "text-destructive",
                    d.status === "running" &&
                      "text-amber-600 dark:text-amber-400",
                    (d.status === "completed" ||
                      d.status === "canceled" ||
                      d.status === "interrupted") &&
                      "text-muted-foreground"
                  )}
                >
                  {t(delegationStatusKey(d.status))}
                </span>
              </div>
              <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5 text-[0.6875rem] text-muted-foreground">
                {d.source_task_id ? (
                  <span>{t("delegationContinuation")}</span>
                ) : null}
                {d.effective_model ? (
                  <span className="min-w-0 truncate">{d.effective_model}</span>
                ) : null}
                {d.duration_ms != null ? (
                  <span className="tabular-nums">
                    {t("roundDurationSeconds", {
                      seconds: Math.max(1, Math.round(d.duration_ms / 1000)),
                    })}
                  </span>
                ) : null}
                {d.input_tokens != null || d.output_tokens != null ? (
                  <span className="tabular-nums">
                    {t("roundTokens", {
                      tokens: (d.input_tokens ?? 0) + (d.output_tokens ?? 0),
                    })}
                  </span>
                ) : null}
                {d.error_code ? (
                  <span className="min-w-0 truncate text-destructive">
                    {t("roundErrorLabel", { code: d.error_code })}
                  </span>
                ) : null}
                <button
                  type="button"
                  className="inline-flex shrink-0 items-center gap-1 text-primary underline-offset-2 hover:underline"
                  disabled={opening === d.child_conversation_id}
                  onClick={() => void openSession(d.child_conversation_id)}
                >
                  {opening === d.child_conversation_id ? (
                    <Loader2
                      className="size-3 animate-spin"
                      aria-hidden="true"
                    />
                  ) : (
                    <MessageSquareText className="size-3" aria-hidden="true" />
                  )}
                  {t("delegationOpenSession")}
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}

function firstLine(text: string): string {
  return (
    text
      .split("\n")
      .find((line) => line.trim() !== "")
      ?.trim() ?? text
  )
}

/** Literal keys so next-intl's typed `t()` accepts them. */
type DelegationStatusKey =
  | "delegationStatusRunning"
  | "delegationStatusCompleted"
  | "delegationStatusFailed"
  | "delegationStatusCanceled"
  | "delegationStatusInterrupted"

function delegationStatusKey(status: string): DelegationStatusKey {
  switch (status) {
    case "running":
      return "delegationStatusRunning"
    case "failed":
      return "delegationStatusFailed"
    case "canceled":
      return "delegationStatusCanceled"
    case "interrupted":
      return "delegationStatusInterrupted"
    case "completed":
    default:
      return "delegationStatusCompleted"
  }
}
