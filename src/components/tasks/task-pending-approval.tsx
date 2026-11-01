"use client"

import { useEffect, useState } from "react"
import { ShieldAlert, Terminal } from "lucide-react"
import { Button } from "@/components/ui/button"
import {
  acpGetSessionSnapshotByConversation,
  acpRespondPermission,
} from "@/lib/api"
import { useAgentVocabulary } from "@/hooks/use-agent-vocabulary"
import { parsePermissionToolCall } from "@/lib/permission-request"
import type { PendingPermissionState, WorkTask } from "@/lib/types"

/**
 * A board card's live answer to "what is it waiting on, exactly?".
 *
 * `awaiting_input` used to be a dead end: the card said the task waited on
 * the user, but the only places that showed WHAT it asked were the child
 * conversation's own tab — which a headless board run has nobody looking at
 * (2026-10-31: two cards sat in `awaiting_input` from second five and were
 * canceled unread). This hook pulls the run conversation's pending permission
 * straight from the session snapshot, so the card itself can render the
 * request and answer it.
 *
 * Polls only while the task is actually `awaiting_input` with a conversation
 * bound; every other state renders nothing and costs nothing. The pull is the
 * same snapshot a conversation tab attaches with, so a permission answered
 * elsewhere clears here on the next tick too.
 */
export function useWorkTaskPendingApproval(
  task: Pick<WorkTask, "status" | "conversation_id">
): PendingPermissionState | null {
  // Keyed by the conversation the pull answered for: a state left behind by a
  // previous task/conversation must never render for this one, and the guard
  // replaces a synchronous reset (an in-effect setState the linter rightly
  // rejects) with a derivation on read.
  const [state, setState] = useState<{
    conversationId: number
    pending: PendingPermissionState | null
  } | null>(null)
  const conversationId = task.conversation_id
  const awaiting = task.status === "awaiting_input"
  useEffect(() => {
    if (!awaiting || conversationId == null) return
    let alive = true
    const pull = async () => {
      const snapshot = await acpGetSessionSnapshotByConversation(
        conversationId
      ).catch(() => null)
      if (alive)
        setState({
          conversationId,
          pending: snapshot?.pending_permission ?? null,
        })
    }
    void pull()
    const timer = setInterval(pull, 2500)
    return () => {
      alive = false
      clearInterval(timer)
    }
  }, [awaiting, conversationId])
  if (!awaiting || conversationId == null) return null
  return state?.conversationId === conversationId ? state.pending : null
}

/**
 * The pending request rendered on the card: enough context to decide (what
 * the agent wants to run) plus its own option buttons, answered in place via
 * the same `acp_respond_permission` the conversation's approval dialog uses.
 * Compact sibling of the chat `PermissionDialog` / pet-panel permission card,
 * scoped to the tasks feature so the board pulls nothing from either.
 */
export function TaskPendingApproval({
  task,
  pending,
}: {
  task: Pick<WorkTask, "connection_id" | "agent_type">
  pending: PendingPermissionState
}) {
  const [busy, setBusy] = useState(false)
  const parsed = parsePermissionToolCall(pending.tool_call)
  const options = useAgentVocabulary(task.agent_type ?? null).permissionOptions(
    pending.options
  )

  // The card lives inside a clickable row/card; answering is not opening.
  const keepLocal = (e: React.MouseEvent) => e.stopPropagation()

  const respond = (optionId: string) => {
    if (busy || task.connection_id == null) return
    setBusy(true)
    void acpRespondPermission(
      task.connection_id,
      pending.request_id,
      optionId
    ).catch((err) => {
      console.warn("[Tasks] respond permission failed:", err)
      setBusy(false)
    })
  }
  // The next poll drops the card when the backend resolves the request; if
  // that event is missed, re-enable rather than strand disabled buttons.
  useEffect(() => {
    if (!busy) return
    const timer = setTimeout(() => setBusy(false), 5000)
    return () => clearTimeout(timer)
  }, [busy])

  const changeSummary =
    parsed.additions > 0 || parsed.deletions > 0
      ? `+${parsed.additions} −${parsed.deletions}`
      : null

  return (
    <div
      className="w-full rounded-md border border-amber-500/30 bg-amber-500/5 p-2"
      onClick={keepLocal}
      onKeyDown={keepLocal}
    >
      <div className="flex items-center gap-1.5 text-xs font-medium">
        <ShieldAlert
          className="h-3.5 w-3.5 shrink-0 text-amber-500"
          aria-hidden="true"
        />
        <span className="truncate">{parsed.description ?? parsed.title}</span>
      </div>
      {parsed.command ? (
        <div className="mt-1 flex items-start gap-1 text-2xs text-muted-foreground">
          <Terminal className="mt-0.5 h-3 w-3 shrink-0" aria-hidden="true" />
          <code className="min-w-0 truncate font-mono">{parsed.command}</code>
        </div>
      ) : changeSummary ? (
        <div className="mt-1 font-mono text-2xs text-muted-foreground">
          {changeSummary}
        </div>
      ) : parsed.contentText ? (
        <div className="mt-1 line-clamp-2 text-2xs text-muted-foreground">
          {parsed.contentText}
        </div>
      ) : null}
      <div className="mt-2 flex flex-wrap gap-1.5">
        {options.map((opt) => {
          const isReject = opt.kind.startsWith("reject")
          return (
            <Button
              key={opt.option_id}
              size="sm"
              variant={isReject ? "outline" : "default"}
              disabled={busy || task.connection_id == null}
              className="h-6 px-2 text-2xs"
              onClick={(e) => {
                e.stopPropagation()
                respond(opt.option_id)
              }}
            >
              {opt.name}
            </Button>
          )
        })}
      </div>
    </div>
  )
}
