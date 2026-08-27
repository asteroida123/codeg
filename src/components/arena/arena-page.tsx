"use client"

import { useCallback, useMemo, useState } from "react"
import { useTranslations } from "next-intl"
import { Plus, Swords } from "lucide-react"

import { Button } from "@/components/ui/button"
import { WorkbenchPageTitle } from "@/components/workbench/workbench-page-title"
import { useArenaView } from "@/contexts/arena-view-context"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import { workTaskGet } from "@/lib/api"
import type { WorkTask, WorkTaskBatchMember } from "@/lib/types"
import { TaskTranscriptDialog } from "@/components/tasks/task-transcript-dialog"
import { requestOpenTaskDetail } from "@/components/tasks/tasks-chrome-actions"
import { useWorkbenchRoute } from "@/contexts/workbench-route-context"
import { ArenaLauncherDialog } from "./arena-launcher-dialog"
import { ArenaRoundCard } from "./arena-round-card"

export function ArenaPageTitle() {
  const t = useTranslations("Arena")
  return <WorkbenchPageTitle title={t("title")} />
}

/**
 * The Arena: run one task with several agents from the same commit, side by
 * side, and read what each produced.
 *
 * Deliberately thin. It renders rounds the backend owns and sends commands back;
 * it holds no orchestration state, drives no agent, and manages no worktree — so
 * closing this window, opening a second one, or attaching a phone leaves a
 * running round exactly as it was. The comparison itself is ordinary work tasks
 * grouped by a `WorkTaskBatch`, which is why every result is reviewable, mergeable
 * and cleanable through the same machinery as any other task.
 *
 * V1 shows deterministic evidence only: live status, the applied configuration
 * with every gap from what was requested, diff size, and each contender's
 * transcript. No model ranks the results, and nothing an agent produced is
 * executed to display it.
 */
export function ArenaPage() {
  const t = useTranslations("Arena")
  const { rounds, loading, refetch } = useArenaView()
  const { setRoute } = useWorkbenchRoute()
  const folders = useAppWorkspaceStore((s) => s.folders)
  const [launcherOpen, setLauncherOpen] = useState(false)
  const [transcriptTask, setTranscriptTask] = useState<WorkTask | null>(null)

  const folderName = useMemo(() => {
    const byId = new Map(folders.map((f) => [f.id, f.alias ?? f.name]))
    return (id: number) => byId.get(id) ?? null
  }, [folders])

  /**
   * Open a contender's transcript.
   *
   * The member row carries the ids but not a whole task, and the transcript
   * viewer takes the task — so it is fetched on demand rather than the round list
   * carrying every member's full task row on every refetch (which happens on
   * every member event, several times a minute during a live round).
   */
  const handleOpenTranscript = useCallback(
    async (member: WorkTaskBatchMember) => {
      try {
        setTranscriptTask(await workTaskGet(member.task_id))
      } catch {
        // The member's task is gone — nothing to show, and the round list will
        // catch up on its next refetch.
      }
    },
    []
  )

  /**
   * Show a contender's changes.
   *
   * Hands the member's task to the board rather than rendering a diff here. The
   * board already owns the diff, the changed-file list, the preflight output, and
   * every action a result can be taken through — merge it, send it back, deliver a
   * pull request. Re-implementing any of that inside the Arena would be a second
   * copy of the product's review surface, and the worse one.
   */
  const handleOpenDiff = useCallback(
    (member: WorkTaskBatchMember) => {
      requestOpenTaskDetail(member.task_id)
      setRoute("tasks")
    },
    [setRoute]
  )

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex shrink-0 items-center gap-2 px-4 py-2">
        <Button size="sm" onClick={() => setLauncherOpen(true)}>
          <Plus />
          {t("newRound")}
        </Button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-4 pb-4">
        {loading ? null : rounds.length === 0 ? (
          <EmptyState onStart={() => setLauncherOpen(true)} />
        ) : (
          <div className="flex flex-col gap-3">
            {rounds.map((round) => (
              <ArenaRoundCard
                key={round.id}
                round={round}
                folderName={folderName(round.folder_id)}
                onOpenTranscript={(m) => void handleOpenTranscript(m)}
                onOpenDiff={handleOpenDiff}
              />
            ))}
          </div>
        )}
      </div>

      <ArenaLauncherDialog
        open={launcherOpen}
        onOpenChange={setLauncherOpen}
        onCreated={() => void refetch()}
      />
      <TaskTranscriptDialog
        open={transcriptTask != null}
        onOpenChange={(open) => {
          if (!open) setTranscriptTask(null)
        }}
        task={transcriptTask}
      />
    </div>
  )
}

function EmptyState({ onStart }: { onStart: () => void }) {
  const t = useTranslations("Arena")
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 text-center">
      <Swords className="size-8 text-muted-foreground/40" aria-hidden="true" />
      <div className="flex flex-col gap-1">
        <p className="text-sm font-medium">{t("empty.title")}</p>
        <p className="max-w-sm text-[0.8125rem] text-muted-foreground">
          {t("empty.description")}
        </p>
      </div>
      <Button size="sm" variant="outline" onClick={onStart}>
        <Plus />
        {t("newRound")}
      </Button>
    </div>
  )
}
