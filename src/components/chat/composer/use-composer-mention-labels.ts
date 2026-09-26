"use client"

import { useMemo } from "react"
import { useTranslations } from "next-intl"

import type { MentionUiLabels } from "./suggestion/types"
import type { ReferenceGroupLabels } from "./use-reference-search"

export interface ComposerMentionLabels {
  /** Group headings, doubling as the panel's tab labels. */
  groupLabels: ReferenceGroupLabels
  /** Empty / loading / listbox name / "more results" / count announcement. */
  uiLabels: MentionUiLabels
}

/**
 * Localized chrome for the `@` reference panel, shared by every host that
 * mounts a {@link RichComposer} (the chat input, the automation editor, the
 * task editor, the task follow-up box). The strings live in the chat composer's
 * namespace because that is where the panel came from — a mention menu reads
 * the same wherever it is opened.
 *
 * `groupLabels` is memoized because `useReferenceSearch` takes it as a
 * dependency of its fetch effect; an inline object would re-run the search on
 * every render.
 */
export function useComposerMentionLabels(): ComposerMentionLabels {
  const t = useTranslations("Folder.chat.messageInput")
  const groupLabels = useMemo<ReferenceGroupLabels>(
    () => ({
      file: t("mentionGroupFile"),
      agent: t("mentionGroupAgent"),
      session: t("mentionGroupSession"),
      commit: t("mentionGroupCommit"),
      skill: t("mentionGroupSkill"),
      delegatedSession: t("mentionGroupDelegatedSession"),
      delegationRound: (rounds) => t("mentionDelegationRound", { rounds }),
      // Explicit switch rather than a template-literal key: the messages are
      // strictly typed from en.json, and this keeps every status key
      // compile-checked against it.
      delegationStatus: (status) => {
        switch (status) {
          case "running":
            return t("mentionDelegationStatus.running")
          case "completed":
            return t("mentionDelegationStatus.completed")
          case "failed":
            return t("mentionDelegationStatus.failed")
          case "canceled":
            return t("mentionDelegationStatus.canceled")
          case "interrupted":
            return t("mentionDelegationStatus.interrupted")
        }
      },
    }),
    [t]
  )
  const uiLabels = useMemo<MentionUiLabels>(
    () => ({
      empty: t("mentionEmpty"),
      loading: t("mentionLoading"),
      listbox: t("mentionListLabel"),
      more: t("mentionMore"),
      count: (count: number) => t("mentionCount", { count }),
    }),
    [t]
  )
  return { groupLabels, uiLabels }
}
