"use client"

import { useTranslations } from "next-intl"
import {
  CircleAlert,
  CircleCheck,
  CircleSlash,
  Clock,
  FileDiff,
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
  /** Open this member's diff. */
  onOpenDiff: (member: WorkTaskBatchMember) => void
}

/**
 * One contender in a round.
 *
 * Shows the configuration that actually took effect, read from the engine's own
 * `config_effective` audit event — not the one that was requested at creation
 * time. The distinction is the card's whole job: a comparison whose
 * configurations were silently adjusted (an effort level the model turns out not
 * to have, a session setting the agent could not honour) is measuring something
 * other than what it claims to. Where request and reality differ, the card says
 * so rather than showing the flattering number.
 *
 * Before the contender launches there is no applied configuration to show — the
 * card falls back to naming what was requested, marked as such, instead of
 * presenting a request as a result.
 */
export function ArenaMemberCard({
  member,
  onOpenTranscript,
  onOpenDiff,
}: ArenaMemberCardProps) {
  const t = useTranslations("Arena")
  const phase = memberPhase(member.task_status)
  const Icon = PHASE_ICON[phase]
  const applied = readApplied(member)
  const requested = readRequested(member)
  const warnings = readWarnings(member)
  const hasDiff = member.files_changed != null
  const preflight = member.preflight

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

      {/* Applied when the engine has resolved it; otherwise the request, labelled
          as a request. Never a request dressed up as a result. */}
      {applied.length > 0 ? (
        <ConfigList entries={applied} />
      ) : requested.length > 0 ? (
        <div className="flex flex-col gap-0.5">
          <span className="text-[0.625rem] uppercase tracking-wide text-muted-foreground/70">
            {t("requestedLabel")}
          </span>
          <ConfigList entries={requested} />
        </div>
      ) : null}

      {/* Every gap between what was requested and what applied. On the card, not
          behind a detail view: it is the difference between a fair comparison and
          one that only looks fair. */}
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

      {/* Preflight: the folder's own check, run against this contender's result.
          The one piece of evidence here that no model produced. */}
      {preflight != null ? (
        <p
          className={cn(
            "flex items-center gap-1.5 text-[0.6875rem]",
            preflight.status === "passed"
              ? "text-emerald-700 dark:text-emerald-500"
              : preflight.status === "failed"
                ? "text-destructive"
                : "text-muted-foreground"
          )}
        >
          {preflight.status === "running" ? (
            <Loader2
              className="size-3 shrink-0 animate-spin"
              aria-hidden="true"
            />
          ) : preflight.status === "passed" ? (
            <CircleCheck className="size-3 shrink-0" aria-hidden="true" />
          ) : (
            <CircleAlert className="size-3 shrink-0" aria-hidden="true" />
          )}
          <span className="min-w-0 truncate">
            {t(`preflight.${preflight.status}`, {
              command: preflight.command,
            })}
          </span>
        </p>
      ) : null}

      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[0.6875rem] text-muted-foreground">
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
        <div className="ml-auto flex shrink-0 items-center gap-2">
          {hasDiff ? (
            <button
              type="button"
              onClick={() => onOpenDiff(member)}
              className="flex items-center gap-1 rounded text-primary underline-offset-2 outline-none hover:underline focus-visible:ring-2 focus-visible:ring-ring"
            >
              <FileDiff className="size-3" aria-hidden="true" />
              {t("openDiff")}
            </button>
          ) : null}
          {member.conversation_id != null ? (
            <button
              type="button"
              onClick={() => onOpenTranscript(member)}
              className="rounded text-primary underline-offset-2 outline-none hover:underline focus-visible:ring-2 focus-visible:ring-ring"
            >
              {t("openTranscript")}
            </button>
          ) : null}
        </div>
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

function ConfigList({ entries }: { entries: [string, string][] }) {
  return (
    <dl className="flex flex-wrap gap-x-3 gap-y-1 text-[0.6875rem] text-muted-foreground">
      {entries.map(([key, value]) => (
        <div key={key} className="flex min-w-0 gap-1">
          <dt className="shrink-0">{key}</dt>
          <dd className="min-w-0 truncate font-mono text-foreground/80">
            {value}
          </dd>
        </div>
      ))}
    </dl>
  )
}

/**
 * The configuration that actually took effect, from the engine's resolved
 * profile.
 *
 * Reads defensively at every level: these are opaque JSON columns and audit
 * payloads, so a shape written by another build must degrade to "nothing to show"
 * rather than throw inside a card the whole round is rendered through.
 */
function readApplied(member: WorkTaskBatchMember): [string, string][] {
  const applied = member.applied_profile?.applied
  return toEntries(applied)
}

/** What was asked for, when nothing has been applied yet. */
function readRequested(member: WorkTaskBatchMember): [string, string][] {
  const requested = (
    member.profile_snapshot?.requested as
      | { config_values?: unknown; mode_id?: unknown }
      | undefined
  )?.config_values
  return toEntries(requested)
}

function toEntries(value: unknown): [string, string][] {
  if (value == null || typeof value !== "object") return []
  const out: [string, string][] = []
  for (const [key, raw] of Object.entries(value as Record<string, unknown>)) {
    if (raw == null) continue
    // Session-level policies resolve to "inherit" for everyone today; printing
    // that on every card is noise, not information.
    if (key.endsWith(".mode") && raw === "inherit") continue
    out.push([key, String(raw)])
  }
  return out
}

function readWarnings(member: WorkTaskBatchMember): string[] {
  const warnings = member.applied_profile?.warnings
  if (!Array.isArray(warnings)) return []
  return warnings.filter((w): w is string => typeof w === "string")
}
