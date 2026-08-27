"use client"

import { useCallback, useMemo, useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { Loader2 } from "lucide-react"

import { workTaskBatchAdopt, workTaskBatchStart } from "@/lib/api"
import { refusedStarts } from "@/lib/work-task-batch-model"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import type { WorkTask } from "@/lib/types"

/** Matches the backend's own cap. */
const MAX_MEMBERS = 16

interface TaskBatchDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** Every task on the board; the dialog narrows to the eligible ones itself. */
  tasks: WorkTask[]
  /** folder id → display name. */
  folderNames: Map<number, string>
  onCreated: () => void
}

/**
 * Run several to-dos together, from one commit.
 *
 * What this adds over pressing Start on each card: the members share a pinned
 * base commit (so their diffs are measured against the same thing), and the group
 * answers to one cancel and one cleanup — with a separate, persisted answer for
 * every member's worktree removal. That last part has no per-task equivalent at
 * all; removing five worktrees one at a time gives five separate confirmations
 * and no record of which ones actually succeeded.
 *
 * Only `todo` tasks with no worktree are offered. A task that already has one
 * started somewhere, so a batch claiming to pin its base would be recording a
 * commit it does not have — the backend refuses those, and there is no reason to
 * let the user pick one first.
 */
export function TaskBatchDialog({
  open,
  onOpenChange,
  tasks,
  folderNames,
  onCreated,
}: TaskBatchDialogProps) {
  const t = useTranslations("TaskBatch")
  const [selected, setSelected] = useState<Set<number>>(new Set())
  const [title, setTitle] = useState("")
  const [submitting, setSubmitting] = useState(false)

  const eligible = useMemo(
    () =>
      tasks.filter(
        (task) =>
          task.status === "todo" &&
          task.worktree_folder_id == null &&
          task.archived_at == null
      ),
    [tasks]
  )

  /** Members share one repository's commit, so a selection cannot span projects.
   *  Grouping the list by folder makes that a visible property of the list rather
   *  than an error the user meets after choosing. */
  const byFolder = useMemo(() => {
    const map = new Map<number, WorkTask[]>()
    for (const task of eligible) {
      const list = map.get(task.folder_id) ?? []
      list.push(task)
      map.set(task.folder_id, list)
    }
    return [...map.entries()]
  }, [eligible])

  /** The folder the current selection belongs to — `null` when nothing is
   *  picked. Every other folder's rows are disabled while it is set. */
  const lockedFolderId = useMemo(() => {
    for (const task of eligible) {
      if (selected.has(task.id)) return task.folder_id
    }
    return null
  }, [eligible, selected])

  const toggle = useCallback((id: number) => {
    setSelected((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }, [])

  const reset = useCallback(() => {
    setSelected(new Set())
    setTitle("")
  }, [])

  const canSubmit =
    lockedFolderId != null &&
    selected.size >= 2 &&
    selected.size <= MAX_MEMBERS &&
    title.trim().length > 0 &&
    !submitting

  const handleSubmit = useCallback(async () => {
    if (!canSubmit || lockedFolderId == null) return
    setSubmitting(true)
    // Selection order is not meaningful; board order is what the user sees, so
    // the slots follow it.
    const taskIds = eligible
      .filter((task) => selected.has(task.id))
      .map((task) => task.id)
    try {
      const batch = await workTaskBatchAdopt(
        lockedFolderId,
        title.trim(),
        taskIds
      )
      // Grouping and starting are one action here: a batch of to-dos the user
      // just chose to "run together" has no meaning as a group that sits still.
      // The two calls stay separate underneath because `adopt` is what pins the
      // base and `start` is what claims the members — and a start that partly
      // refuses must still leave the group intact.
      const outcomes = await workTaskBatchStart(batch.id)
      const refused = refusedStarts(outcomes)
      if (refused.length === 0) {
        toast.success(t("toasts.started", { count: outcomes.length }))
      } else {
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
      onCreated()
      onOpenChange(false)
      reset()
    } catch (e) {
      // The backend's refusal names the task and the reason — pass it through
      // rather than replacing it with a generic failure.
      toast.error(t("toasts.failed"), { description: String(e) })
    } finally {
      setSubmitting(false)
    }
  }, [
    canSubmit,
    lockedFolderId,
    eligible,
    selected,
    title,
    onCreated,
    onOpenChange,
    reset,
    t,
  ])

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("title")}</DialogTitle>
          <DialogDescription>{t("description")}</DialogDescription>
        </DialogHeader>

        {eligible.length === 0 ? (
          <p className="py-4 text-[0.8125rem] text-muted-foreground">
            {t("noEligible")}
          </p>
        ) : (
          <div className="flex flex-col gap-3">
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="task-batch-title">{t("batchName")}</Label>
              <Input
                id="task-batch-title"
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                placeholder={t("batchNamePlaceholder")}
              />
            </div>

            <div className="flex flex-col gap-1.5">
              <Label>{t("pickTasks")}</Label>
              <div className="max-h-64 overflow-y-auto rounded-xl border">
                {byFolder.map(([folderId, folderTasks]) => {
                  const otherProject =
                    lockedFolderId != null && lockedFolderId !== folderId
                  return (
                    <div key={folderId}>
                      <div className="flex items-baseline gap-2 border-b bg-muted/40 px-3 py-1.5">
                        <span className="truncate text-[0.6875rem] font-medium text-muted-foreground">
                          {folderNames.get(folderId) ?? `#${folderId}`}
                        </span>
                        {otherProject ? (
                          <span className="shrink-0 text-[0.625rem] text-muted-foreground/70">
                            {t("otherProject")}
                          </span>
                        ) : null}
                      </div>
                      {folderTasks.map((task) => (
                        <label
                          key={task.id}
                          className="flex cursor-pointer items-center gap-2 px-3 py-1.5 text-[0.8125rem] hover:bg-accent/50 has-disabled:cursor-not-allowed has-disabled:opacity-40"
                        >
                          <input
                            type="checkbox"
                            checked={selected.has(task.id)}
                            disabled={otherProject}
                            onChange={() => toggle(task.id)}
                          />
                          <span className="min-w-0 truncate">{task.title}</span>
                        </label>
                      ))}
                    </div>
                  )
                })}
              </div>
              <p className="text-[0.6875rem] text-muted-foreground">
                {t("pickTasksHint", { max: MAX_MEMBERS })}
              </p>
            </div>
          </div>
        )}

        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            {t("cancel")}
          </Button>
          <Button disabled={!canSubmit} onClick={handleSubmit}>
            {submitting ? <Loader2 className="animate-spin" /> : null}
            {t("run", { count: selected.size })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
