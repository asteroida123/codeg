import type { FlatFileEntry } from "@/hooks/use-file-tree"
import { formatConversationTitle } from "@/lib/conversation-title"
import { buildFileUri } from "@/lib/reference-link"
import {
  type AcpAgentInfo,
  type DbConversationSummary,
  type DelegatedChildSession,
  type DelegatedChildStatus,
  type GitLogEntry,
} from "@/lib/types"
import { getAgentLabel } from "@/lib/custom-agents"
import { formatRelative } from "@/components/conversations/sidebar-conversation-grouping"

import type { SuggestionItem } from "./types"

function joinPath(root: string, relative: string): string {
  const left = root.replace(/[/\\]+$/, "")
  const right = relative.replace(/^[/\\]+/, "")
  return left ? `${left}/${right}` : right
}

/** Workspace file → file reference (uri built from the workspace root). */
export function fileToSuggestion(
  entry: FlatFileEntry,
  workspaceRoot: string
): SuggestionItem {
  return {
    reference: {
      refType: "file",
      id: entry.relativePath,
      label: entry.name,
      uri: buildFileUri(joinPath(workspaceRoot, entry.relativePath)),
      meta: { fileKind: entry.kind },
    },
    detail: entry.relativePath,
    keywords: entry.relativePath,
  }
}

/**
 * ACP agent → agent reference. Carries a `codeg://agent/<agent_type>` uri as a
 * routing anchor: it serializes inline as `[@label](codeg://agent/…)` and
 * renders as a badge in the transcript. The readable link IS the routing
 * anchor: the backend derives the delegation reminder from it at send time, so
 * nothing has to travel out-of-band alongside the prompt.
 */
export function agentToSuggestion(agent: AcpAgentInfo): SuggestionItem {
  return {
    reference: {
      refType: "agent",
      id: agent.agent_type,
      label: agent.name || getAgentLabel(agent.agent_type),
      uri: `codeg://agent/${agent.agent_type}`,
      meta: { agentType: agent.agent_type, available: agent.available },
    },
    detail: agent.description || null,
    keywords: agent.agent_type,
  }
}

/**
 * Conversation → session reference. The serialization uri encodes codeg's
 * internal numeric conversation id as `codeg://session/<conversation_id>` — the
 * stable key the `get_session_info` MCP tool resolves directly (it then reads the
 * row's bound `external_id` + `agent_type` server-side). The `@`-panel option row
 * still shows the owning agent's icon via `meta.agentType`; the inline session
 * badge shows a neutral conversation glyph, not the agent icon.
 */
export function sessionToSuggestion(
  conversation: DbConversationSummary
): SuggestionItem {
  // Fold any inline reference badges in the title (`[name](file://…)`, …) down
  // to their bracket text, so the panel row and the inserted session badge read
  // like the sidebar's title (`README.md fix`, not raw `[README.md](…)`) rather
  // than leaking serialized Markdown. The numeric `#id` fallback also covers a
  // whitespace-only title (folding can't turn blank into non-blank).
  const label =
    formatConversationTitle(conversation.title).trim() || `#${conversation.id}`
  const uri = `codeg://session/${conversation.id}`
  return {
    reference: {
      refType: "session",
      id: String(conversation.id),
      label,
      uri,
      meta: {
        agentType: conversation.agent_type,
        status: conversation.status,
        branch: conversation.git_branch,
      },
    },
    detail: conversation.git_branch || conversation.status,
    keywords: `${label} ${conversation.agent_type}`,
  }
}

/**
 * Git commit → commit reference (`codeg://commit/<repoKey>@<fullHash>`).
 * `repoKey` identifies the repository (e.g. its path) and is URI-encoded.
 */
export function commitToSuggestion(
  entry: GitLogEntry,
  repoKey: string
): SuggestionItem {
  return {
    reference: {
      refType: "commit",
      id: entry.full_hash,
      label: entry.hash,
      uri: `codeg://commit/${encodeURIComponent(repoKey)}@${entry.full_hash}`,
      meta: {
        shortHash: entry.hash,
        message: entry.message,
        author: entry.author,
        pushed: entry.pushed,
      },
    },
    detail: entry.message,
    keywords: `${entry.hash} ${entry.message} ${entry.author}`,
  }
}

/** Localized pieces a delegated-child row's detail line is composed from. */
export interface DelegatedSessionRowLabels {
  /** "Round 3" — the child's admitted round count. */
  round: (rounds: number) => string
  /** Status chip text per wire status (running / completed / … ). */
  status: (status: DelegatedChildStatus) => string
}

/**
 * Delegated child session → session reference. Same insertion payload as
 * {@link sessionToSuggestion} (`codeg://session/<id>`, the stable key
 * `get_session_info` resolves) — the only difference is the row's detail line,
 * which packs the §14.4 affordances: round count, projected status, branch,
 * last activity. `keywords` includes the latest task text so querying the
 * task finds the child that ran it.
 */
export function delegatedSessionToSuggestion(
  child: DelegatedChildSession,
  labels: DelegatedSessionRowLabels,
  now: number
): SuggestionItem {
  const label =
    formatConversationTitle(child.title).trim() ||
    `#${child.child_conversation_id}`
  const activity = formatRelative(child.last_activity_at, now)
  const detail = [
    labels.round(child.rounds),
    labels.status(child.status),
    child.git_branch || "",
    activity,
  ]
    .filter(Boolean)
    .join(" · ")
  return {
    reference: {
      refType: "session",
      id: String(child.child_conversation_id),
      label,
      uri: `codeg://session/${child.child_conversation_id}`,
      meta: {
        agentType: child.agent_type,
        status: child.status,
        branch: child.git_branch,
      },
    },
    detail,
    keywords: `${label} ${child.agent_type} ${child.latest_task} ${child.git_branch ?? ""}`,
  }
}

/**
 * §14.4 display order for the delegated-children group: 运行中 (running) →
 * 最近完成且可继续 (terminal + continuable) → 需要恢复 (interrupted) →
 * 已关闭/其他 (everything else, e.g. a terminal round still awaiting its
 * release ack). Within a bucket, most recent activity first, then the higher
 * child id, so the order is total and stable.
 */
export function compareDelegatedSessions(
  left: DelegatedChildSession,
  right: DelegatedChildSession
): number {
  const rankDiff = delegatedRank(left) - delegatedRank(right)
  if (rankDiff !== 0) return rankDiff
  const activityDiff =
    Date.parse(right.last_activity_at) - Date.parse(left.last_activity_at)
  if (activityDiff !== 0) return activityDiff
  return right.child_conversation_id - left.child_conversation_id
}

function delegatedRank(child: DelegatedChildSession): number {
  if (child.status === "running") return 0
  if (child.continuable && child.status !== "interrupted") return 1
  if (child.status === "interrupted") return 2
  return 3
}

// Skills, commands and experts are no longer surfaced in the `@` panel — they
// are inserted via the `/` and `$` triggers, which build their reference attrs
// directly (see composer/invocation-reference.ts).
