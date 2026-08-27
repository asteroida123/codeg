"use client"

import { useTranslations } from "next-intl"
import {
  CircleAlert,
  CircleCheck,
  CircleSlash,
  Clock,
  Loader2,
  MessageCircleQuestion,
} from "lucide-react"

import { cn } from "@/lib/utils"
import type { WorkTaskBatchMember } from "@/lib/types"
import {
  memberLabel,
  memberPhase,
  type MemberPhase,
} from "@/lib/work-task-batch-model"

const PHASE_ICON: Record<MemberPhase, typeof Clock> = {
  waiting: Clock,
  working: Loader2,
  blocked: MessageCircleQuestion,
  finished: CircleCheck,
  stopped: CircleSlash,
}

const PHASE_TINT: Record<MemberPhase, string> = {
  waiting: "text-muted-foreground",
  working: "text-primary",
  // Amber, like every other "waiting on you" signal in the product: it is not a
  // fault, and colouring it destructive would send the user looking for a break.
  blocked: "text-amber-600 dark:text-amber-500",
  finished: "text-emerald-600 dark:text-emerald-500",
  stopped: "text-muted-foreground",
}

interface ArenaMemberCardProps {
  member: WorkTaskBatchMember
  /** Open this member's transcript. */
  onOpenTranscript: (member: WorkTaskBatchMember) => void
}

/**
 * One contender in a round.
 *
 * Shows the configuration that was actually APPLIED, not the one that was asked
 * for. `profile_snapshot` carries both, and a comparison whose configurations
 * were silently adjusted — an effort level the model does not have, a session
 * setting the agent could not honour — is measuring something other than what it
 * claims to. Where the two differ, the card says so rather than showing the
 * flattering number.
 */
export function ArenaMemberCard({
  member,
  onOpenTranscript,
}: ArenaMemberCardProps) {
  const t = useTranslations("Arena")
  const phase = memberPhase(member.task_status)
  const Icon = PHASE_ICON[phase]
  const applied = readApplied(member)
  const warnings = readWarnings(member)
  const hasDiff = member.files_changed != null

  return (
    <div className="flex min-w-0 flex-col gap-2 rounded-xl border bg-card p-3">
      <div className="flex min-w-0 items-center gap-2">
        <Icon
          className={cn(
            "size-3.5 shrink-0",
            PHASE_TINT[phase],
            phase === "working" && "animate-spin"
          )}
          aria-hidden="true"
        />
        <span className="min-w-0 flex-1 truncate text-[0.8125rem] font-medium">
          {memberLabel(member)}
        </span>
        <span className="shrink-0 text-[0.6875rem] text-muted-foreground">
          {t(`phase.${phase}`)}
        </span>
      </div>

      {applied.length > 0 ? (
        <dl className="flex flex-wrap gap-x-3 gap-y-1 text-[0.6875rem] text-muted-foreground">
          {applied.map(([key, value]) => (
            <div key={key} className="flex min-w-0 gap-1">
              <dt className="shrink-0">{key}</dt>
              <dd className="min-w-0 truncate font-mono text-foreground/80">
                {value}
              </dd>
            </div>
          ))}
        </dl>
      ) : null}

      {/* Every gap between what was requested and what applied. Shown on the
          card, not tucked behind a detail view: it is the difference between a
          fair comparison and one that only looks fair. */}
      {warnings.length > 0 ? (
        <ul className="flex flex-col gap-1">
          {warnings.map((warning) => (
            <li
              key={warning}
              className="flex gap-1.5 text-[0.6875rem] leading-snug text-amber-700 dark:text-amber-500"
            >
              <CircleAlert
                className="mt-0.5 size-3 shrink-0"
                aria-hidden="true"
              />
              <span className="min-w-0">{warning}</span>
            </li>
          ))}
        </ul>
      ) : null}

      <div className="flex items-center gap-3 text-[0.6875rem] text-muted-foreground">
        {hasDiff ? (
          <span className="font-mono">
            {t("diffStat", {
              files: member.files_changed ?? 0,
              additions: member.additions ?? 0,
              deletions: member.deletions ?? 0,
            })}
          </span>
        ) : (
          <span>{t("noChangesYet")}</span>
        )}
        {member.conversation_id != null ? (
          <button
            type="button"
            onClick={() => onOpenTranscript(member)}
            className="ml-auto rounded text-primary underline-offset-2 outline-none hover:underline focus-visible:ring-2 focus-visible:ring-ring"
          >
            {t("openTranscript")}
          </button>
        ) : null}
      </div>

      {/* A cleanup that did not succeed stays on the card until it does. This is
          the visible half of the persisted per-member ledger — the failure that
          used to be swallowed. */}
      {member.cleanup_result === "failed" ||
      member.cleanup_result === "blocked" ? (
        <p className="rounded-lg bg-muted/60 px-2 py-1.5 text-[0.6875rem] leading-snug text-muted-foreground">
          {member.cleanup_result === "blocked"
            ? t("cleanupBlocked")
            : t("cleanupFailed", {
                reason: member.cleanup_error ?? t("cleanupNoReason"),
              })}
        </p>
      ) : null}
    </div>
  )
}

/** The configuration that actually took effect, as label/value pairs. Reads the
 *  snapshot's `applied` map defensively: it is an opaque JSON column, so a shape
 *  from another build must degrade to "nothing to show" rather than throw. */
function readApplied(member: WorkTaskBatchMember): [string, string][] {
  const applied = member.profile_snapshot?.applied
  if (applied == null || typeof applied !== "object") return []
  const out: [string, string][] = []
  for (const [key, value] of Object.entries(
    applied as Record<string, unknown>
  )) {
    if (value == null) continue
    // Session-level policies resolve to "inherit" for everyone today; printing
    // that on every card is noise, not information.
    if (key.endsWith(".mode") && value === "inherit") continue
    out.push([key, String(value)])
  }
  return out
}

function readWarnings(member: WorkTaskBatchMember): string[] {
  const warnings = member.profile_snapshot?.warnings
  if (!Array.isArray(warnings)) return []
  return warnings.filter((w): w is string => typeof w === "string")
}
