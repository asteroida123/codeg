"use client"

import { cn } from "@/lib/utils"
import type { NavBadgeProps } from "@/lib/workbench/contributions"

/**
 * Marks the Arena as still-settling work, drawn on the entry points that lead
 * there. Marking only the page would tell people after they had already
 * committed to the click.
 *
 * Its own component rather than Forge's identical one: two features sharing a
 * badge means one cannot graduate out of beta without touching the other, and a
 * `ForgeBetaBadge` import inside the Arena is exactly the kind of quiet coupling
 * the contribution registry exists to avoid.
 *
 * The word stays "Beta" in every locale we ship, so it is a literal rather than a
 * message key — ten identical entries plus a parity test to keep them that way
 * buys nothing. Deliberately not `aria-hidden`: "Arena Beta" is what a screen
 * reader should announce, the same way the count chips on the neighbouring rows
 * read as part of their row.
 */
export function ArenaBetaBadge({ className }: Partial<NavBadgeProps>) {
  return (
    <span
      className={cn(
        // Same chip metrics as the sidebar's shortcut and count badges, so this
        // sits on their rail instead of reading as a third kind of ornament.
        "inline-flex h-[0.9375rem] shrink-0 items-center justify-center",
        "rounded-[0.3125rem] bg-primary/10 px-[0.3125rem]",
        "text-[0.625rem] font-medium leading-none text-primary",
        className
      )}
    >
      Beta
    </span>
  )
}
