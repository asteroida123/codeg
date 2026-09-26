"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { CircleAlert, Loader2, RotateCw } from "lucide-react"
import { workTaskRuns } from "@/lib/api"
import { onTransportReconnect, subscribe } from "@/lib/platform"
import { formatTokenCount } from "@/lib/token-format"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import type { WorkTask, WorkTaskRun } from "@/lib/types"

const WORK_TASK_CHANGED_EVENT = "task://changed"

interface TaskRunRoundsProps {
  /** The host drawer's open state: the sections stop reading while it is
   *  closed (the content stays mounted after its first open). */
  open: boolean
  task: WorkTask
  /** The explicit way out of a `resume_failed` stop: retry with a NEW session.
   *  Omitted means the banner's action is not offered. */
  onRunWithNewSession?: () => void
  /** A retry is already in flight (the banner's action is disabled). */
  busy?: boolean
}

/**
 * The task's execution generations — one row per `work_task_run`, newest
 * first. This is where "the rework continued the same session instead of
 * silently starting over" is visible rather than asserted: each round names
 * its kind, how the previous session was continued, and what it cost.
 *
 * A generation whose strict continuation was refused (`failure_reason ==
 * "resume_failed"`) leads with its own banner: that failure is different from
 * every other one — nothing ran — and it has exactly one remedy, the explicit
 * new-session retry the banner offers.
 */
export function TaskRunRounds({
  open,
  task,
  onRunWithNewSession,
  busy = false,
}: TaskRunRoundsProps) {
  const t = useTranslations("Tasks")
  const [runs, setRuns] = useState<WorkTaskRun[]>([])
  /** Ordering guard for overlapping reads (see `reload`). */
  const reqRef = useRef(0)
  const taskId = task.id
  // Any transition that settles or opens a generation moves one of these.
  const reloadKey = `${task.run_seq}:${task.status}`

  const reload = useCallback(async () => {
    const seq = ++reqRef.current
    try {
      const next = await workTaskRuns(taskId)
      // A slower earlier read must not overwrite a newer one.
      if (seq !== reqRef.current) return
      setRuns(next)
    } catch {
      // Keep the previous list on a transient read failure.
    }
  }, [taskId])

  useEffect(() => {
    if (!open) return
    let unsub: (() => void) | undefined
    let cancelled = false
    void subscribe(WORK_TASK_CHANGED_EVENT, () => {
      void reload()
    }).then((u: () => void) => {
      if (cancelled) u()
      else unsub = u
    })
    const offReconnect = onTransportReconnect(() => {
      void reload()
    })
    // The initial read is the same async fetch every broadcast makes; the
    // lint rule cannot see through the transport call and reads it as a
    // synchronous setState.
    /* eslint-disable react-hooks/set-state-in-effect */
    void reload()
    /* eslint-enable react-hooks/set-state-in-effect */
    return () => {
      cancelled = true
      unsub?.()
      offReconnect?.()
    }
  }, [open, reload, reloadKey])

  const resumeFailed = task.failure_reason === "resume_failed"

  return (
    <section className="flex flex-col gap-1.5">
      {resumeFailed ? (
        <div className="flex flex-col gap-2 rounded-xl border border-destructive/30 bg-destructive/5 p-3">
          <div className="flex items-start gap-2 text-xs text-destructive">
            <CircleAlert
              className="mt-0.5 size-3.5 shrink-0"
              aria-hidden="true"
            />
            <div className="flex min-w-0 flex-col gap-1">
              <span className="font-medium">{t("resumeFailedTitle")}</span>
              <span className="whitespace-pre-wrap break-words">
                {t("resumeFailedBody")}
              </span>
            </div>
          </div>
          {onRunWithNewSession ? (
            <Button
              type="button"
              size="sm"
              variant="outline"
              className="self-start"
              disabled={busy}
              onClick={onRunWithNewSession}
            >
              {busy ? (
                <Loader2 className="size-3.5 animate-spin" aria-hidden="true" />
              ) : (
                <RotateCw className="size-3.5" aria-hidden="true" />
              )}
              {t("resumeFailedAction")}
            </Button>
          ) : null}
        </div>
      ) : null}

      <h3 className="text-[0.6875rem] font-medium uppercase tracking-wide text-muted-foreground">
        {t("detailRounds")}
      </h3>
      {runs.length === 0 ? (
        <p className="text-xs text-muted-foreground">
          {t("detailRoundsEmpty")}
        </p>
      ) : (
        <ol className="flex flex-col divide-y divide-border/60 overflow-hidden rounded-xl border border-border">
          {runs.map((run) => (
            <li key={run.id} className="flex flex-col gap-1 px-2.5 py-2">
              <div className="flex min-w-0 items-center gap-2 text-xs">
                <span className="shrink-0 font-medium tabular-nums">
                  {t("roundLabel", { seq: run.run_seq })}
                </span>
                <span className="min-w-0 shrink truncate text-muted-foreground">
                  {t(roundKindKey(run.kind))}
                </span>
                <span className="flex-1" />
                <span
                  className={cn(
                    "shrink-0 text-[0.6875rem]",
                    run.status === "failed" && "text-destructive",
                    run.status === "running" &&
                      "text-amber-600 dark:text-amber-400",
                    (run.status === "settled" || run.status === "canceled") &&
                      "text-muted-foreground"
                  )}
                >
                  {t(roundStatusKey(run.status))}
                </span>
              </div>
              <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5 text-[0.6875rem] text-muted-foreground">
                {run.resume_outcome ? (
                  <span
                    className={cn(
                      run.resume_outcome === "strict_failed" &&
                        "text-destructive"
                    )}
                  >
                    {t(resumeOutcomeKey(run.resume_outcome))}
                  </span>
                ) : null}
                {run.duration_ms != null ? (
                  <span className="tabular-nums">
                    {t("roundDurationSeconds", {
                      seconds: Math.max(1, Math.round(run.duration_ms / 1000)),
                    })}
                  </span>
                ) : null}
                {run.input_tokens != null || run.output_tokens != null ? (
                  <span className="tabular-nums">
                    {t("roundTokens", {
                      tokens: formatTokenCount(
                        (run.input_tokens ?? 0) + (run.output_tokens ?? 0)
                      ),
                    })}
                  </span>
                ) : null}
                {run.verdict ? (
                  <span className="min-w-0 truncate">
                    {t("roundVerdict", { verdict: run.verdict })}
                  </span>
                ) : null}
                {run.error_code ? (
                  <span className="min-w-0 truncate text-destructive">
                    {t("roundErrorLabel", { code: run.error_code })}
                  </span>
                ) : null}
              </div>
              {run.external_session_id ? (
                <span
                  className="min-w-0 truncate font-mono text-[0.625rem] text-muted-foreground/80"
                  title={run.external_session_id}
                >
                  {run.external_session_id}
                </span>
              ) : null}
            </li>
          ))}
        </ol>
      )}
    </section>
  )
}

/** Literal keys so next-intl's typed `t()` accepts them. */
type RoundKindKey =
  | "roundKindFresh"
  | "roundKindRetry"
  | "roundKindReturn"
  | "roundKindMerge"

type RoundStatusKey =
  | "roundStatusRunning"
  | "roundStatusSettled"
  | "roundStatusFailed"
  | "roundStatusCanceled"

type ResumeOutcomeKey =
  | "roundResumeResumed"
  | "roundResumeFreshRequested"
  | "roundResumeFallbackCold"
  | "roundResumeStrictFailed"
  | "roundResumeFreshNoSession"

function roundKindKey(kind: string): RoundKindKey {
  switch (kind) {
    case "fresh":
      return "roundKindFresh"
    case "retry":
      return "roundKindRetry"
    case "return":
      return "roundKindReturn"
    case "merge":
      return "roundKindMerge"
    default:
      return "roundKindFresh"
  }
}

function roundStatusKey(status: string): RoundStatusKey {
  switch (status) {
    case "running":
      return "roundStatusRunning"
    case "settled":
      return "roundStatusSettled"
    case "failed":
      return "roundStatusFailed"
    case "canceled":
      return "roundStatusCanceled"
    default:
      return "roundStatusSettled"
  }
}

function resumeOutcomeKey(outcome: string): ResumeOutcomeKey {
  switch (outcome) {
    case "resumed":
      return "roundResumeResumed"
    case "fresh_requested":
      return "roundResumeFreshRequested"
    case "fallback_cold":
      return "roundResumeFallbackCold"
    case "strict_failed":
      return "roundResumeStrictFailed"
    case "fresh_no_session":
    default:
      return "roundResumeFreshNoSession"
  }
}
