"use client"

import { useTranslations } from "next-intl"
import { Kanban, Layers, List, Settings2 } from "lucide-react"
import { Button } from "@/components/ui/button"
import { useTasksView } from "@/contexts/tasks-view-context"
import type { WorkbenchChromeActionsProps } from "@/components/workbench/workbench-content"

/** Fired by the chrome cluster's settings button; TasksPage owns the dialog
 *  (and the folder filter that scopes it), so the button just asks it to open.
 *  Lives here rather than in tasks-page.tsx because the sender is in the window
 *  chrome and the receiver is the page — neither should import the other. */
export const OPEN_TASK_SETTINGS_EVENT = "codeg:open-task-settings"

/** Same hand-off for the "run several to-dos together" dialog: the button is in
 *  the window chrome, the dialog belongs to the page that owns the task list. */
export const OPEN_TASK_BATCH_EVENT = "codeg:open-task-batch"

/**
 * The Tasks route's own entries in the window's top-right chrome cluster,
 * rendered immediately left of the (window-level) settings gear — see
 * `WorkbenchRouteChromeActions`.
 *
 * They cost nothing in width: a full-page route hides the terminal and aux
 * toggles (they act on the workspace this route covers), so these two take
 * their place and the cluster keeps its three-button reservation
 * (RIGHT_CHROME_CLUSTER).
 */
export function TasksChromeActions({
  buttonClassName,
  iconClassName,
}: WorkbenchChromeActionsProps) {
  const t = useTranslations("Tasks")
  const tBatch = useTranslations("TaskBatch")
  const { tasks, viewMode, setViewMode } = useTasksView()
  // Same eligibility the dialog applies: a task with a worktree already has a
  // starting commit, so a batch cannot pin one for it.
  const eligibleForBatch = tasks.filter(
    (task) =>
      task.status === "todo" &&
      task.worktree_folder_id == null &&
      task.archived_at == null
  ).length
  // A single toggle rather than a segmented pair, and it shows the mode it
  // would switch TO: on one button "what happens if I press this" is the only
  // reading that doesn't need a legend.
  const toList = viewMode === "board"
  const switchLabel = t(toList ? "viewSwitchToList" : "viewSwitchToBoard")

  return (
    <>
      {/* Withheld on an empty board, like the page's own toolbar: with no tasks
          there is nothing to lay out either way. */}
      {tasks.length > 0 ? (
        <Button
          variant="ghost"
          size="icon"
          className={buttonClassName}
          onClick={() => setViewMode(toList ? "list" : "board")}
          title={switchLabel}
          aria-label={switchLabel}
        >
          {toList ? (
            <List className={iconClassName} />
          ) : (
            <Kanban className={iconClassName} />
          )}
        </Button>
      ) : null}
      {/* Offered only when there is something to group. Two, not one: a "batch"
          of one is just the task, and its extra machinery would buy nothing. */}
      {eligibleForBatch >= 2 ? (
        <Button
          variant="ghost"
          size="icon"
          className={buttonClassName}
          onClick={() => window.dispatchEvent(new Event(OPEN_TASK_BATCH_EVENT))}
          title={tBatch("title")}
          aria-label={tBatch("title")}
        >
          <Layers className={iconClassName} />
        </Button>
      ) : null}
      <Button
        variant="ghost"
        size="icon"
        className={buttonClassName}
        onClick={() =>
          window.dispatchEvent(new Event(OPEN_TASK_SETTINGS_EVENT))
        }
        title={t("settingsTitle")}
        aria-label={t("settingsTitle")}
      >
        <Settings2 className={iconClassName} />
      </Button>
    </>
  )
}
