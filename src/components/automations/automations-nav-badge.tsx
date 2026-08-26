"use client"

import { useAutomationsView } from "@/contexts/automations-view-context"
import { cn } from "@/lib/utils"
import type { NavBadgeProps } from "@/lib/workbench/contributions"

/**
 * Count of automation runs that failed since the user last looked.
 *
 * Lives with the feature rather than inline in the sidebar: the count comes from
 * the automations context, and a sidebar that rendered it directly would have to
 * subscribe to every feature's context to draw any row. Registered as this
 * route's `trailing` contribution instead, so the sidebar renders a component it
 * knows nothing about.
 *
 * Destructive tint, unlike the neighbouring rows: a failure is not a queue.
 */
export function AutomationsNavBadge({ className }: NavBadgeProps) {
  const { unseenFailures } = useAutomationsView()
  if (unseenFailures <= 0) return null
  return (
    <span
      className={cn(
        "inline-flex h-[0.9375rem] min-w-[0.9375rem] shrink-0 items-center justify-center",
        "rounded-full bg-destructive/15 px-1",
        "font-mono text-[0.625rem] font-medium leading-none text-destructive",
        className
      )}
    >
      {unseenFailures}
    </span>
  )
}
