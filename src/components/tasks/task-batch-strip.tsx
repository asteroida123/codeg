"use client"

import { useCallback, useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { GitCommitHorizontal, ListX, Loader2, Trash2, X } from "lucide-react"

import {
  workTaskBatchCancel,
  workTaskBatchCleanup,
  workTaskBatchDelete,
} from "@/lib/api"
import {
  batchControls,
  refusedStarts,
  summarizeCleanup,
} from "@/lib/work-task-batch-model"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import type { WorkTaskBatch } from "@/lib/types"

interface TaskBatchStripProps {
  batches: WorkTaskBatch[]
  folderNames: Map<number, string>
}

/**
 * Aggregate controls for the batches a user made on this board.
 *
 * Without this the dialog would be a half-feature: a group you can form and
 * start but not stop or clean up as a group — which is precisely the "you can
 * look but not manage" shape worth avoiding. One cancel, one cleanup, and a
 * per-member answer for each removal.
 *
 * Reuses the shared batch model (`@/lib/work-task-batch-model`) rather than
 * re-deriving the rules, so the board and the Arena cannot disagree about when a
 * cleanup may be offered or what counts as a success. The model lives in `lib/`
 * precisely so neither feature owns it.
 */
export function TaskBatchStrip({ batches, folderNames }: TaskBatchStripProps) {
  if (batches.length === 0) return null
  return (
    <div className="flex shrink-0 flex-col gap-1.5 px-4 pb-1">
      {batches.map((batch) => (
        <BatchRow
          key={batch.id}
          batch={batch}
          folderName={folderNames.get(batch.folder_id) ?? null}
        />
      ))}
    </div>
  )
}

function BatchRow({
  batch,
  folderName,
}: {
  batch: WorkTaskBatch
  folderName: string | null
}) {
  const t = useTranslations("TaskBatch")
  const [busy, setBusy] = useState<"cancel" | "cleanup" | "dismiss" | null>(
    null
  )
  const controls = batchControls(batch)

  const handleCancel = useCallback(async () => {
    setBusy("cancel")
    try {
      const outcomes = await workTaskBatchCancel(batch.id)
      const refused = refusedStarts(outcomes)
      if (refused.length === 0) {
        // Worktrees survive, exactly as they do for a single task's cancel.
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
  }, [batch.id, t])

  const handleCleanup = useCallback(async () => {
    setBusy("cleanup")
    try {
      const outcomes = await workTaskBatchCleanup(batch.id)
      const summary = summarizeCleanup(outcomes)
      if (summary.allSucceeded) {
        toast.success(t("toasts.cleanedUp", { count: summary.succeeded }))
      } else {
        // Anything short of every member succeeding is reported as such, with the
        // backend's own reasons. This is the answer the board could not give when
        // worktrees were removed one at a time.
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
  }, [batch.id, t])

  // Dropping the grouping row — the exit for a batch that is over and fully
  // cleaned. Without it a finished group would sit in the strip forever:
  // nothing else ever removes it. Soft-delete only; the member tasks and
  // anything still on disk are not touched (the model only offers this once
  // `cleanableCount` reaches zero), and the backend's `Deleted` event makes
  // every listener refetch.
  const handleDismiss = useCallback(async () => {
    setBusy("dismiss")
    try {
      await workTaskBatchDelete(batch.id)
    } catch (e) {
      toast.error(String(e))
      setBusy(null)
    }
    // No success toast and no setBusy(null) on purpose: the row is about to
    // unmount with the refetch the backend event triggers.
  }, [batch.id])

  return (
    <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 rounded-xl border bg-muted/30 px-3 py-1.5">
      <span className="min-w-0 max-w-[16rem] truncate text-[0.8125rem] font-medium">
        {batch.title}
      </span>
      <span
        className={cn(
          "shrink-0 rounded-full px-1.5 py-0.5 text-[0.625rem] font-medium",
          batch.status === "canceled"
            ? "bg-muted text-muted-foreground"
            : batch.status === "review"
              ? "bg-amber-500/10 text-amber-700 dark:text-amber-500"
              : batch.status === "settled"
                ? "bg-emerald-500/10 text-emerald-700 dark:text-emerald-500"
                : batch.status === "running"
                  ? "bg-primary/10 text-primary"
                  : "bg-muted text-muted-foreground"
        )}
      >
        {t(`status.${batch.status}`)}
      </span>
      <span className="shrink-0 text-[0.6875rem] text-muted-foreground">
        {t("memberCount", { count: batch.members.length })}
      </span>
      {folderName ? (
        <span className="min-w-0 truncate text-[0.6875rem] text-muted-foreground">
          {folderName}
        </span>
      ) : null}
      {/* The shared starting commit — the reason the members' diffs are
          comparable, and the one fact a per-task view cannot show. */}
      <span className="flex shrink-0 items-center gap-1 font-mono text-[0.6875rem] text-muted-foreground">
        <GitCommitHorizontal className="size-3" aria-hidden="true" />
        {batch.base_branch}@{batch.base_sha.slice(0, 7)}
      </span>

      <div className="ml-auto flex shrink-0 items-center gap-1">
        {controls.canCancel ? (
          <Button
            size="sm"
            variant="ghost"
            className="h-6 px-2 text-[0.6875rem]"
            disabled={busy != null}
            onClick={handleCancel}
          >
            {busy === "cancel" ? <Loader2 className="animate-spin" /> : <X />}
            {t("cancelAll")}
          </Button>
        ) : null}
        {controls.canCleanup ? (
          <Button
            size="sm"
            variant="ghost"
            className="h-6 px-2 text-[0.6875rem]"
            disabled={busy != null}
            onClick={handleCleanup}
          >
            {busy === "cleanup" ? (
              <Loader2 className="animate-spin" />
            ) : (
              <Trash2 />
            )}
            {t("cleanUpAll", { count: controls.cleanableCount })}
          </Button>
        ) : null}
        {controls.canDismiss ? (
          <Button
            size="sm"
            variant="ghost"
            className="h-6 px-2 text-[0.6875rem] text-muted-foreground"
            disabled={busy != null}
            onClick={handleDismiss}
          >
            {busy === "dismiss" ? (
              <Loader2 className="animate-spin" />
            ) : (
              <ListX />
            )}
            {t("dismiss")}
          </Button>
        ) : null}
      </div>
    </div>
  )
}
