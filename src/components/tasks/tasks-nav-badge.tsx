"use client"

import { useTasksView } from "@/contexts/tasks-view-context"
import { cn } from "@/lib/utils"
import type { NavBadgeProps } from "@/lib/workbench/contributions"

/**
 * Count of tasks waiting on the user — a review to accept, a question to answer.
 *
 * Primary tint, not destructive: this is a queue, not a fault. The neighbouring
 * automations badge is the other way round for exactly that reason.
 *
 * Lives with the feature rather than inline in the sidebar, so the sidebar does
 * not have to subscribe to the tasks context to draw its own rows.
 */
export function TasksNavBadge({ className }: NavBadgeProps) {
  const { attentionCount } = useTasksView()
  if (attentionCount <= 0) return null
  return (
    <span
      className={cn(
        "inline-flex h-[0.9375rem] min-w-[0.9375rem] shrink-0 items-center justify-center",
        "rounded-full bg-primary/10 px-1",
        "font-mono text-[0.625rem] font-medium leading-none text-primary",
        className
      )}
    >
      {attentionCount}
    </span>
  )
}
