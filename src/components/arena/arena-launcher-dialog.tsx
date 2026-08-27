"use client"

import { useCallback, useEffect, useMemo, useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { Loader2, Plus, Trash2 } from "lucide-react"

import { workTaskBatchCreate, workTaskBatchStart } from "@/lib/api"
import { AGENT_LABELS, type WorkTaskBatchSpec } from "@/lib/types"
import { refusedStarts } from "@/lib/work-task-batch-model"
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
import {
  AgentConfigSection,
  effectiveSelections,
  snapshotLabels,
} from "@/components/automations/agent-config-section"
import { useAgentOptions } from "@/components/automations/use-agent-options"
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
  /** Chosen agent mode, or null to take the agent's own current one. */
  modeId: string | null
  /** Option id → value id, exactly as the agent advertised them. `model` and
   *  `reasoning_effort` are ordinary entries here — there is no special case. */
  configValues: Record<string, string>
}

let slotKeySeq = 0
function newSlot(agentType: string): SlotDraft {
  return { key: slotKeySeq++, agentType, modeId: null, configValues: {} }
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
 * Each contender's model, mode and reasoning level come from the agent itself —
 * `useAgentOptions` probes what it advertises and `AgentConfigSection` renders
 * it, the same pair the task editor uses. This used to be a free-text model box,
 * which was indefensible on two counts: the user had to already know the exact
 * model id, and a typo produced a round that silently ran on the default while
 * claiming to compare something else.
 *
 * The base commit is deliberately NOT a field. It is resolved server-side from
 * the folder's HEAD at create time, so a client cannot name a commit the
 * repository never had — and cannot race the branch switch the pin exists to
 * survive.
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

  const folderPath = useMemo(
    () => projectFolders.find((f) => String(f.id) === folderId)?.path ?? null,
    [projectFolders, folderId]
  )

  const patchSlot = useCallback((index: number, patch: Partial<SlotDraft>) => {
    setSlots((prev) =>
      prev.map((slot, i) => (i === index ? { ...slot, ...patch } : slot))
    )
  }, [])

  const reset = useCallback(() => {
    setFolderId("")
    setTitle("")
    setObjective("")
    setSlots([newSlot("claude_code"), newSlot("codex")])
    setAllowDirty(false)
  }, [])

  /**
   * Each slot's probe result, published upward by the slot rows.
   *
   * A ref-like Map rather than state: it is read only when the form is submitted,
   * and making it state would re-render the whole dialog every time one of up to
   * four probes lands — a flicker per arrival, for a value nothing renders.
   */
  const slotSnapshots = useMemo(
    () => new Map<number, Parameters<typeof effectiveSelections>[0]>(),
    []
  )
  /** Stable across renders, so a row's publish effect fires when its probe lands
   *  rather than on every keystroke in the objective field. */
  const publishSnapshot = useCallback(
    (key: number, snapshot: Parameters<typeof effectiveSelections>[0]) => {
      slotSnapshots.set(key, snapshot)
    },
    [slotSnapshots]
  )

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
    const roundTitle = title.trim()
    const spec: WorkTaskBatchSpec = {
      folder_id: Number(folderId),
      title: roundTitle,
      owner_extension: ARENA_OWNER_EXTENSION,
      allow_dirty: allowDirty,
      members: slots.map((slot) => {
        const agentLabel =
          AGENT_LABELS[slot.agentType as keyof typeof AGENT_LABELS] ??
          slot.agentType
        // The values the user actually SAW, with each untouched select filled
        // from the option's own current value — so the round pins concrete
        // configuration instead of empty overrides that resolve later.
        const resolved = effectiveSelections(
          slotSnapshots.get(slot.key) ?? null,
          slot.modeId,
          slot.configValues
        )
        const labels = snapshotLabels(
          slotSnapshots.get(slot.key) ?? null,
          slot.modeId,
          slot.configValues
        )
        const model = resolved.config_values.model
        const label = `${agentLabel}${model ? ` · ${model}` : ""}`
        return {
          title: `${roundTitle} — ${label}`,
          label,
          config: {
            // Every contender gets the SAME prompt, byte for byte. A round that
            // varied the wording per slot would be comparing the prompts.
            display_text: prompt,
            prompt_blocks: [{ type: "text", text: prompt }],
            agent_type: slot.agentType as never,
            mode_id: resolved.mode_id,
            config_values: resolved.config_values,
            label_snapshot: labels,
          },
          // What was REQUESTED. What actually applied is written by the engine at
          // launch and read back from the task's `config_effective` event — the
          // card shows both, because the gap between them is the honest part.
          profile_snapshot: {
            requested: {
              agent_type: slot.agentType,
              mode_id: resolved.mode_id,
              config_values: resolved.config_values,
            },
          },
        }
      }),
      metadata: { slots: slots.length, createdBy: "arena.launcher" },
    }

    try {
      const batch = await workTaskBatchCreate(spec)
      // Create and start are one action: a round the user just configured has no
      // meaning as a group that sits still, and the task board's bulk grouping
      // behaves the same way. They stay separate calls underneath because create
      // is what pins the base and start is what claims the members — a start that
      // partly refuses must still leave the round intact.
      const outcomes = await workTaskBatchStart(batch.id)
      const refused = refusedStarts(outcomes)
      if (refused.length === 0) {
        toast.success(t("toasts.created", { count: slots.length }))
      } else {
        toast.warning(
          t("toasts.startedPartially", {
            started: outcomes.length - refused.length,
            refused: refused.length,
          }),
          {
            description: refused
              .map((r) => r.error)
              .filter(Boolean)
              .join("\n"),
          }
        )
      }
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
    slotSnapshots,
    t,
  ])

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>{t("launcher.title")}</DialogTitle>
          <DialogDescription>{t("launcher.description")}</DialogDescription>
        </DialogHeader>

        <div className="flex max-h-[60vh] flex-col gap-3 overflow-y-auto">
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
              <SlotRow
                key={slot.key}
                slot={slot}
                folderPath={folderPath}
                canRemove={slots.length > MIN_SLOTS}
                onPatch={(patch) => patchSlot(i, patch)}
                onRemove={() => setSlots((s) => s.filter((_, xi) => xi !== i))}
                slotKey={slot.key}
                onSnapshot={publishSnapshot}
              />
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

/**
 * One contender row: the agent, then whatever that agent says it can be
 * configured with.
 *
 * Its own component because each row needs its own `useAgentOptions` call, and
 * hooks cannot be called in a loop. That also means each row probes
 * independently — picking Codex for slot 2 does not re-probe slot 1.
 */
function SlotRow({
  slot,
  slotKey,
  folderPath,
  canRemove,
  onPatch,
  onRemove,
  onSnapshot,
}: {
  slot: SlotDraft
  slotKey: number
  folderPath: string | null
  canRemove: boolean
  onPatch: (patch: Partial<SlotDraft>) => void
  onRemove: () => void
  onSnapshot: (
    key: number,
    snapshot: Parameters<typeof effectiveSelections>[0]
  ) => void
}) {
  const t = useTranslations("Arena")
  // Held until a project is chosen: the probe runs the agent in a working
  // directory, and its answer can differ per repository.
  const { snapshot, loading, error, reload } = useAgentOptions(
    slot.agentType,
    folderPath,
    folderPath != null
  )
  // Publish upward for the submit handler. In an effect, not during render:
  // writing to the parent's Map while rendering is a side effect React is free to
  // repeat, and "free to repeat" is not a property worth relying on even when the
  // write happens to be idempotent.
  useEffect(() => {
    onSnapshot(slotKey, snapshot)
  }, [slotKey, snapshot, onSnapshot])

  return (
    <div className="flex flex-col gap-1.5 rounded-xl border p-2">
      <div className="flex items-center gap-1.5">
        <Select
          value={slot.agentType}
          onValueChange={(value) =>
            // A different agent advertises different options, so the old
            // selections cannot carry over — keeping them would submit values
            // the new agent never offered.
            onPatch({ agentType: value, modeId: null, configValues: {} })
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
        <div className="min-w-0 flex-1" />
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="shrink-0"
          disabled={!canRemove}
          aria-label={t("launcher.removeContender")}
          onClick={onRemove}
        >
          <Trash2 />
        </Button>
      </div>

      {folderPath == null ? (
        <p className="px-1 text-[0.6875rem] text-muted-foreground">
          {t("launcher.pickFolderFirst")}
        </p>
      ) : (
        // Model, mode and reasoning level all come from here — they are ordinary
        // config options the agent advertised. An agent that advertises none
        // renders nothing rather than an empty control.
        <AgentConfigSection
          snapshot={snapshot}
          loading={loading}
          error={error}
          onReload={reload}
          modeId={slot.modeId}
          configValues={slot.configValues}
          onModeChange={(modeId) => onPatch({ modeId })}
          onConfigChange={(optionId, valueId) => {
            const next = { ...slot.configValues }
            if (valueId == null) delete next[optionId]
            else next[optionId] = valueId
            onPatch({ configValues: next })
          }}
          layout="inline"
        />
      )}
    </div>
  )
}
