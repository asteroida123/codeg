"use client"

import { useCallback, useMemo, useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { Loader2, Plus, Trash2 } from "lucide-react"

import { workTaskBatchCreate } from "@/lib/api"
import { AGENT_LABELS, type WorkTaskBatchSpec } from "@/lib/types"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Textarea } from "@/components/ui/textarea"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import { ARENA_OWNER_EXTENSION } from "@/contexts/arena-view-context"

/** Slots a round may hold.
 *
 * Four, not eight. Each contender is a full worktree — a complete checkout of the
 * repository — plus an agent process, so the cost is real; and four side-by-side
 * results is already past what anyone reads carefully. The backend's own limit is
 * higher and unchanged; this is a product judgement, not a constraint. */
const MIN_SLOTS = 2
const MAX_SLOTS = 4

interface SlotDraft {
  key: number
  agentType: string
  /** Blank = the agent's own default. */
  model: string
  label: string
}

let slotKeySeq = 0
function newSlot(agentType: string): SlotDraft {
  return { key: slotKeySeq++, agentType, model: "", label: "" }
}

const AGENT_OPTIONS = Object.entries(AGENT_LABELS)

interface ArenaLauncherDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** Refetch the round list after a successful create. */
  onCreated: () => void
}

/**
 * Sets up a comparison round: one objective, one folder, and two to four
 * contenders.
 *
 * The base commit is deliberately NOT a field. It is resolved server-side from
 * the folder's HEAD at create time, so a client cannot name a commit the
 * repository never had — and cannot race the branch switch the pin exists to
 * survive. If the repository is empty, on a detached HEAD, or has uncommitted
 * tracked changes, the backend refuses and says which; the dirty case can be
 * accepted knowingly, and even then nothing is ever committed on the user's
 * behalf.
 */
export function ArenaLauncherDialog({
  open,
  onOpenChange,
  onCreated,
}: ArenaLauncherDialogProps) {
  const t = useTranslations("Arena")
  const folders = useAppWorkspaceStore((s) => s.folders)
  const projectFolders = useMemo(
    // A round's members each get their own worktree of the project, so the
    // target has to be a project root — a worktree folder would nest them.
    () => folders.filter((f) => f.parent_id == null),
    [folders]
  )

  const [folderId, setFolderId] = useState<string>("")
  const [title, setTitle] = useState("")
  const [objective, setObjective] = useState("")
  const [slots, setSlots] = useState<SlotDraft[]>(() => [
    newSlot("claude_code"),
    newSlot("codex"),
  ])
  const [allowDirty, setAllowDirty] = useState(false)
  const [submitting, setSubmitting] = useState(false)

  const reset = useCallback(() => {
    setFolderId("")
    setTitle("")
    setObjective("")
    setSlots([newSlot("claude_code"), newSlot("codex")])
    setAllowDirty(false)
  }, [])

  const canSubmit =
    folderId !== "" &&
    title.trim().length > 0 &&
    objective.trim().length > 0 &&
    slots.length >= MIN_SLOTS &&
    !submitting

  const handleSubmit = useCallback(async () => {
    if (!canSubmit) return
    setSubmitting(true)
    const prompt = objective.trim()
    const spec: WorkTaskBatchSpec = {
      folder_id: Number(folderId),
      title: title.trim(),
      owner_extension: ARENA_OWNER_EXTENSION,
      allow_dirty: allowDirty,
      members: slots.map((slot) => {
        const agentLabel =
          AGENT_LABELS[slot.agentType as keyof typeof AGENT_LABELS] ??
          slot.agentType
        const model = slot.model.trim()
        const label =
          slot.label.trim() || `${agentLabel}${model ? ` · ${model}` : ""}`
        const configValues: Record<string, string> = {}
        // Blank means "the agent's own default" — an empty string here would be
        // recorded as a requested model of "", which the profile snapshot would
        // then faithfully report.
        if (model) configValues.model = model
        return {
          title: `${title.trim()} — ${label}`,
          label,
          config: {
            // Every contender gets the SAME prompt, byte for byte. A round that
            // varied the wording per slot would be comparing the prompts.
            display_text: prompt,
            prompt_blocks: [{ type: "text", text: prompt }],
            agent_type: slot.agentType as never,
            config_values: configValues,
            mode_id: null,
          },
        }
      }),
      metadata: { slots: slots.length, createdBy: "arena.launcher" },
    }

    try {
      await workTaskBatchCreate(spec)
      toast.success(t("toasts.created", { count: slots.length }))
      onCreated()
      onOpenChange(false)
      reset()
    } catch (e) {
      // The backend's refusal is the message: it names the repository state that
      // blocked the round and what to do about it.
      toast.error(t("toasts.createFailed"), { description: String(e) })
    } finally {
      setSubmitting(false)
    }
  }, [
    canSubmit,
    folderId,
    title,
    objective,
    slots,
    allowDirty,
    onCreated,
    onOpenChange,
    reset,
    t,
  ])

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("launcher.title")}</DialogTitle>
          <DialogDescription>{t("launcher.description")}</DialogDescription>
        </DialogHeader>

        <div className="flex flex-col gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="arena-folder">{t("launcher.folder")}</Label>
            <Select value={folderId} onValueChange={setFolderId}>
              <SelectTrigger id="arena-folder">
                <SelectValue placeholder={t("launcher.folderPlaceholder")} />
              </SelectTrigger>
              <SelectContent>
                {projectFolders.map((folder) => (
                  <SelectItem key={folder.id} value={String(folder.id)}>
                    {folder.alias ?? folder.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <div className="flex flex-col gap-1.5">
            <Label htmlFor="arena-title">{t("launcher.roundTitle")}</Label>
            <Input
              id="arena-title"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder={t("launcher.roundTitlePlaceholder")}
            />
          </div>

          <div className="flex flex-col gap-1.5">
            <Label htmlFor="arena-objective">{t("launcher.objective")}</Label>
            <Textarea
              id="arena-objective"
              value={objective}
              onChange={(e) => setObjective(e.target.value)}
              placeholder={t("launcher.objectivePlaceholder")}
              rows={4}
            />
            <p className="text-[0.6875rem] text-muted-foreground">
              {t("launcher.objectiveHint")}
            </p>
          </div>

          <div className="flex flex-col gap-2">
            <div className="flex items-center justify-between">
              <Label>{t("launcher.contenders")}</Label>
              <Button
                type="button"
                size="sm"
                variant="ghost"
                disabled={slots.length >= MAX_SLOTS}
                onClick={() => setSlots((s) => [...s, newSlot("claude_code")])}
              >
                <Plus />
                {t("launcher.addContender")}
              </Button>
            </div>
            {slots.map((slot, i) => (
              <div key={slot.key} className="flex items-center gap-1.5">
                <Select
                  value={slot.agentType}
                  onValueChange={(value) =>
                    setSlots((s) =>
                      s.map((x, xi) =>
                        xi === i ? { ...x, agentType: value } : x
                      )
                    )
                  }
                >
                  <SelectTrigger className="w-[11rem] shrink-0">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {AGENT_OPTIONS.map(([value, label]) => (
                      <SelectItem key={value} value={value}>
                        {label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <Input
                  value={slot.model}
                  onChange={(e) =>
                    setSlots((s) =>
                      s.map((x, xi) =>
                        xi === i ? { ...x, model: e.target.value } : x
                      )
                    )
                  }
                  placeholder={t("launcher.modelPlaceholder")}
                  className="min-w-0 flex-1"
                />
                <Button
                  type="button"
                  size="icon"
                  variant="ghost"
                  className="shrink-0"
                  disabled={slots.length <= MIN_SLOTS}
                  aria-label={t("launcher.removeContender")}
                  onClick={() => setSlots((s) => s.filter((_, xi) => xi !== i))}
                >
                  <Trash2 />
                </Button>
              </div>
            ))}
            {/* The same agent twice with different models is a legitimate — and
                the most controlled — round, so nothing here deduplicates. */}
            <p className="text-[0.6875rem] text-muted-foreground">
              {t("launcher.contendersHint")}
            </p>
          </div>

          <label className="flex items-start gap-2 text-[0.75rem] text-muted-foreground">
            <input
              type="checkbox"
              checked={allowDirty}
              onChange={(e) => setAllowDirty(e.target.checked)}
              className="mt-0.5"
            />
            <span>{t("launcher.allowDirty")}</span>
          </label>
        </div>

        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            {t("launcher.cancel")}
          </Button>
          <Button disabled={!canSubmit} onClick={handleSubmit}>
            {submitting ? <Loader2 className="animate-spin" /> : null}
            {t("launcher.create")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
