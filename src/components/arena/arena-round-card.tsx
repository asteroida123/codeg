"use client"

import { useCallback, useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { GitCommitHorizontal, Loader2, Play, Trash2, X } from "lucide-react"

import {
  workTaskBatchCancel,
  workTaskBatchCleanup,
  workTaskBatchStart,
} from "@/lib/api"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import type { WorkTaskBatch, WorkTaskBatchMember } from "@/lib/types"
import { ArenaMemberCard } from "./arena-member-card"
import {
  refusedStarts,
  roundControls,
  roundDiffTotals,
  summarizeCleanup,
} from "./arena-round-model"

interface ArenaRoundCardProps {
  round: WorkTaskBatch
  folderName: string | null
  onOpenTranscript: (member: WorkTaskBatchMember) => void
}

type Busy = "start" | "cancel" | "cleanup" | null

/**
 * One comparison round: its shared base, its contenders, and the three aggregate
 * commands.
 *
 * Every button here sends a backend command and then says exactly what the
 * backend answered — per member. Nothing is inferred, and no outcome is
 * summarized into a success the backend did not report. That is the deliberate
 * counter-design to the implementation this replaces, whose cleanup reported
 * success while every worktree stayed on disk.
 */
export function ArenaRoundCard({
  round,
  folderName,
  onOpenTranscript,
}: ArenaRoundCardProps) {
  const t = useTranslations("Arena")
  const [busy, setBusy] = useState<Busy>(null)
  const controls = roundControls(round)
  const totals = roundDiffTotals(round)

  const handleStart = useCallback(async () => {
    setBusy("start")
    try {
      const outcomes = await workTaskBatchStart(round.id)
      const refused = refusedStarts(outcomes)
      if (refused.length === 0) {
        toast.success(t("toasts.started", { count: outcomes.length }))
      } else {
        // A partial start is reported as partial, naming the reason the backend
        // gave. Collapsing it into "started" would tell the user a contender is
        // running when it is not.
        toast.warning(
          t("toasts.startedPartially", {
            started: outcomes.length - refused.length,
            refused: refused.length,
          }),
          {
            description: refused
              .map((r) => r.error)
              .filter(Boolean)
              .join("\n"),
          }
        )
      }
    } catch (e) {
      toast.error(t("toasts.startFailed"), { description: String(e) })
    } finally {
      setBusy(null)
    }
  }, [round.id, t])

  const handleCancel = useCallback(async () => {
    setBusy("cancel")
    try {
      const outcomes = await workTaskBatchCancel(round.id)
      const refused = refusedStarts(outcomes)
      if (refused.length === 0) {
        // Worktrees survive a cancel, exactly as they do for a single task —
        // said out loud so nobody goes looking for lost work.
        toast.success(t("toasts.canceled"), {
          description: t("toasts.canceledKeepsWorktrees"),
        })
      } else {
        toast.warning(t("toasts.canceledPartially"), {
          description: refused
            .map((r) => r.error)
            .filter(Boolean)
            .join("\n"),
        })
      }
    } catch (e) {
      toast.error(t("toasts.cancelFailed"), { description: String(e) })
    } finally {
      setBusy(null)
    }
  }, [round.id, t])

  const handleCleanup = useCallback(async () => {
    setBusy("cleanup")
    try {
      const outcomes = await workTaskBatchCleanup(round.id)
      const summary = summarizeCleanup(outcomes)
      if (summary.allSucceeded) {
        toast.success(t("toasts.cleanedUp", { count: summary.succeeded }))
      } else {
        // The load-bearing branch. Anything short of every member succeeding is
        // reported as such, with the backend's own reasons, and the member cards
        // keep showing it until a retry clears them.
        toast.warning(
          t("toasts.cleanupIncomplete", {
            succeeded: summary.succeeded,
            failed: summary.failed,
            blocked: summary.blocked,
          }),
          {
            description: summary.unresolved
              .map((o) => o.error)
              .filter(Boolean)
              .join("\n"),
            duration: 10_000,
          }
        )
      }
    } catch (e) {
      toast.error(t("toasts.cleanupFailed"), { description: String(e) })
    } finally {
      setBusy(null)
    }
  }, [round.id, t])

  return (
    <section className="flex flex-col gap-3 rounded-2xl border bg-background p-4">
      <header className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1.5">
        <h2 className="min-w-0 flex-1 truncate text-sm font-semibold">
          {round.title}
        </h2>
        <span
          className={cn(
            "shrink-0 rounded-full px-2 py-0.5 text-[0.6875rem] font-medium",
            round.status === "canceled"
              ? "bg-muted text-muted-foreground"
              : round.status === "review"
                ? "bg-amber-500/10 text-amber-700 dark:text-amber-500"
                : round.status === "settled"
                  ? "bg-emerald-500/10 text-emerald-700 dark:text-emerald-500"
                  : round.status === "running"
                    ? "bg-primary/10 text-primary"
                    : "bg-muted text-muted-foreground"
          )}
        >
          {t(`status.${round.status}`)}
        </span>
      </header>

      <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-[0.6875rem] text-muted-foreground">
        {folderName ? <span className="truncate">{folderName}</span> : null}
        {/* The shared starting commit, stated on the round rather than buried:
            it is the reason the results are comparable at all. */}
        <span className="flex items-center gap-1 font-mono">
          <GitCommitHorizontal className="size-3 shrink-0" aria-hidden="true" />
          {round.base_branch}@{round.base_sha.slice(0, 7)}
        </span>
        {totals.filesChanged > 0 ? (
          <span className="font-mono">
            {t("diffStat", {
              files: totals.filesChanged,
              additions: totals.additions,
              deletions: totals.deletions,
            })}
          </span>
        ) : null}
      </div>

      <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-3">
        {round.members.map((member) => (
          <ArenaMemberCard
            key={member.id}
            member={member}
            onOpenTranscript={onOpenTranscript}
          />
        ))}
      </div>

      <footer className="flex flex-wrap items-center gap-2">
        {controls.canStart ? (
          <Button
            size="sm"
            variant="outline"
            disabled={busy != null}
            onClick={handleStart}
          >
            {busy === "start" ? <Loader2 className="animate-spin" /> : <Play />}
            {t("actions.start")}
          </Button>
        ) : null}
        {controls.canCancel ? (
          <Button
            size="sm"
            variant="outline"
            disabled={busy != null}
            onClick={handleCancel}
          >
            {busy === "cancel" ? <Loader2 className="animate-spin" /> : <X />}
            {t("actions.cancel")}
          </Button>
        ) : null}
        {controls.canCleanup ? (
          <Button
            size="sm"
            variant="outline"
            disabled={busy != null}
            onClick={handleCleanup}
          >
            {busy === "cleanup" ? (
              <Loader2 className="animate-spin" />
            ) : (
              <Trash2 />
            )}
            {t("actions.cleanup", { count: controls.cleanableCount })}
          </Button>
        ) : null}
      </footer>
    </section>
  )
}
