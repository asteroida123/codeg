"use client"

import {
  ChartColumn,
  LayoutTemplate,
  ListTodo,
  Swords,
  Zap,
} from "lucide-react"

import {
  registerNavigationItem,
  registerWorkbenchView,
  type Dispose,
} from "@/lib/workbench/contributions"
import {
  AutomationsPage,
  AutomationsPageTitle,
} from "@/components/automations/automations-page"
import { ForgeChromeActions } from "@/components/forge/forge-chrome-actions"
import { ForgePage, ForgePageTitle } from "@/components/forge/forge-page"
import { TasksChromeActions } from "@/components/tasks/tasks-chrome-actions"
import { TasksPage, TasksPageTitle } from "@/components/tasks/tasks-page"
import {
  TokenUsagePage,
  TokenUsagePageTitle,
} from "@/components/token-usage/token-usage-page"
import { AutomationsNavBadge } from "@/components/automations/automations-nav-badge"
import { TasksNavBadge } from "@/components/tasks/tasks-nav-badge"
import { ForgeBetaBadge } from "@/components/forge/forge-beta-badge"
import { ArenaPage, ArenaPageTitle } from "@/components/arena/arena-page"
import { ArenaBetaBadge } from "@/components/arena/arena-beta-badge"

/**
 * Route ids of the first-party workbench pages.
 *
 * Declared here, beside the registrations, rather than in a union inside the
 * registry — that is the whole point of the seam. A feature that lives entirely
 * in its own directory declares its id from its own file the same way.
 */
declare module "@/lib/workbench/contributions" {
  interface WorkbenchRoutes {
    automations: true
    tasks: true
    forge: true
    tokenUsage: true
    arena: true
  }
}

/**
 * Register every page that ships with codeg.
 *
 * Called once, before the workspace first renders. Idempotent: registering the
 * same id twice replaces it, so a double invocation (a remount, a hot reload)
 * leaves the registry in the same state rather than doubling the sidebar.
 *
 * The order of the `order` values, not of the calls, decides the sidebar layout —
 * so inserting a page between two others does not mean reshuffling this file.
 */
export function activateFirstPartyModules(): Dispose {
  const disposers: Dispose[] = [
    registerWorkbenchView({
      id: "automations",
      page: AutomationsPage,
      strip: AutomationsPageTitle,
    }),
    registerNavigationItem({
      id: "automations",
      icon: Zap,
      labelKey: "automations",
      order: 10,
      trailing: AutomationsNavBadge,
    }),

    registerWorkbenchView({
      id: "tasks",
      page: TasksPage,
      strip: TasksPageTitle,
      chromeActions: TasksChromeActions,
    }),
    registerNavigationItem({
      id: "tasks",
      icon: ListTodo,
      labelKey: "tasks",
      order: 20,
      trailing: TasksNavBadge,
    }),

    registerWorkbenchView({
      id: "forge",
      page: ForgePage,
      strip: ForgePageTitle,
      chromeActions: ForgeChromeActions,
    }),
    registerNavigationItem({
      id: "forge",
      icon: LayoutTemplate,
      labelKey: "forge",
      order: 30,
      trailing: ForgeBetaBadge,
    }),

    registerWorkbenchView({
      id: "arena",
      page: ArenaPage,
      strip: ArenaPageTitle,
    }),
    registerNavigationItem({
      id: "arena",
      icon: Swords,
      labelKey: "arena",
      order: 35,
      trailing: ArenaBetaBadge,
    }),

    registerWorkbenchView({
      id: "tokenUsage",
      page: TokenUsagePage,
      strip: TokenUsagePageTitle,
    }),
    // Reachable from the status bar's counter and the quick-actions menu, but not
    // a permanent sidebar row: it is a report you open, not a place you work.
    // No label key, and none is invented — nothing renders a row for it, so a
    // message that only ever went unused would be dead weight in ten locales.
    registerNavigationItem({
      id: "tokenUsage",
      icon: ChartColumn,
      order: 40,
      inSidebar: false,
    }),
  ]

  return () => {
    for (const dispose of disposers.reverse()) dispose()
  }
}
