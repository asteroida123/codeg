"use client"

/**
 * Sub-agent performance panel (upstream #724): one read-only aggregation of
 * the delegation ledger — all-up totals plus by-agent and by-effective-model
 * breakdowns.
 *
 * Deliberately plain tables, not charts: the point of the MVP is that the
 * data EXISTS and is queryable per agent / model; once #731's evaluation
 * loop builds on it, a real visualization can replace this without touching
 * the backend shape.
 *
 * Every rate is computed by the backend over TERMINAL rows only (running
 * tasks are neither success nor failure); "reworked" counts tasks that a
 * successor delegation continued on the same child.
 */

import { useEffect, useState } from "react"
import { useTranslations } from "next-intl"

import {
  getDelegationPerformance,
  type DelegationDimensionStats,
  type DelegationPerformanceReport,
} from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import { getAgentLabel } from "@/lib/custom-agents"
import type { AgentType } from "@/lib/types"

function percent(rate: number): string {
  return `${Math.round(rate * 1000) / 10}%`
}

function formatCount(n: number): string {
  return n.toLocaleString()
}

function formatDuration(ms: number | null): string {
  if (ms == null) return "—"
  if (ms < 1000) return `${Math.round(ms)}ms`
  if (ms < 10_000) return `${(ms / 1000).toFixed(1)}s`
  const totalSec = Math.round(ms / 1000)
  if (totalSec < 60) return `${totalSec}s`
  return `${Math.floor(totalSec / 60)}m ${totalSec % 60}s`
}

function dimensionLabel(
  key: string | null,
  t: ReturnType<typeof useTranslations>
): string {
  if (key == null) return t("notRecorded")
  // Agent buckets arrive as agent slugs; model buckets are free-form ids
  // that `getAgentLabel` passes through unchanged when it doesn't know them.
  return getAgentLabel(key as AgentType)
}

function StatsTable({
  rows,
  labelKey,
}: {
  rows: DelegationDimensionStats[]
  labelKey: "agent" | "model"
}) {
  const t = useTranslations("AcpAgentSettings.multiAgent.performance")
  return (
    <table className="w-full text-xs">
      <thead>
        <tr className="border-b text-muted-foreground">
          <th className="py-1.5 pr-3 text-left font-medium">
            {labelKey === "agent" ? t("colAgent") : t("colModel")}
          </th>
          <th className="px-2 py-1.5 text-right font-medium">
            {t("colTasks")}
          </th>
          <th className="px-2 py-1.5 text-right font-medium">
            {t("colSuccess")}
          </th>
          <th className="px-2 py-1.5 text-right font-medium">
            {t("colAvgDuration")}
          </th>
          <th className="px-2 py-1.5 text-right font-medium">
            {t("colTokens")}
          </th>
          <th className="px-2 py-1.5 text-right font-medium">
            {t("colRework")}
          </th>
        </tr>
      </thead>
      <tbody>
        {rows.map((row) => (
          <tr key={row.key ?? "__none__"} className="border-b last:border-0">
            <td className="py-1.5 pr-3">{dimensionLabel(row.key, t)}</td>
            <td className="px-2 py-1.5 text-right tabular-nums">
              {formatCount(row.task_count)}
            </td>
            <td className="px-2 py-1.5 text-right tabular-nums">
              {percent(row.success_rate)}
            </td>
            <td className="px-2 py-1.5 text-right tabular-nums">
              {formatDuration(row.avg_duration_ms)}
            </td>
            <td className="px-2 py-1.5 text-right tabular-nums">
              {row.input_tokens + row.output_tokens > 0
                ? formatCount(row.input_tokens + row.output_tokens)
                : "—"}
            </td>
            <td className="px-2 py-1.5 text-right tabular-nums">
              {percent(row.rework_rate)}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

export function DelegationPerformancePanel() {
  const t = useTranslations("AcpAgentSettings.multiAgent.performance")
  const [report, setReport] = useState<DelegationPerformanceReport | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    void getDelegationPerformance()
      .then((r) => {
        if (!cancelled) {
          setReport(r)
          setError(null)
        }
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(toErrorMessage(err))
      })
    return () => {
      cancelled = true
    }
  }, [])

  if (error) {
    return (
      <p className="text-xs text-destructive">
        {t("loadFailed", { detail: error })}
      </p>
    )
  }
  if (!report) {
    return <p className="text-xs text-muted-foreground">{t("loading")}</p>
  }
  if (report.totals.task_count === 0) {
    return <p className="text-xs text-muted-foreground">{t("empty")}</p>
  }

  const totals = report.totals
  return (
    <div className="space-y-4">
      <dl className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-6">
        {(
          [
            ["totalTasks", formatCount(totals.task_count)],
            ["totalSuccess", percent(totals.success_rate)],
            ["totalRework", percent(totals.rework_rate)],
            ["totalAvgDuration", formatDuration(totals.avg_duration_ms)],
            [
              "totalTokens",
              totals.input_tokens + totals.output_tokens > 0
                ? formatCount(totals.input_tokens + totals.output_tokens)
                : "—",
            ],
            ["totalRunning", formatCount(totals.running)],
          ] as const
        ).map(([key, value]) => (
          <div key={key} className="rounded-md border px-3 py-2">
            <dt className="text-2xs text-muted-foreground">{t(key)}</dt>
            <dd className="mt-0.5 text-sm font-medium tabular-nums">{value}</dd>
          </div>
        ))}
      </dl>

      <section>
        <h4 className="mb-1 text-xs font-medium">{t("byAgent")}</h4>
        <StatsTable rows={report.by_agent} labelKey="agent" />
      </section>

      <section>
        <h4 className="mb-1 text-xs font-medium">{t("byModel")}</h4>
        <StatsTable rows={report.by_model} labelKey="model" />
      </section>

      <p className="text-2xs text-muted-foreground">{t("footnote")}</p>
    </div>
  )
}
