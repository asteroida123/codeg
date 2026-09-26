//! `create_automation` / `create_work_task` backing logic + settings
//! persistence.
//!
//! Two surfaces live here, mirroring `crate::commands::session_info`:
//!
//!   * [`DbChatAuthoring`] — the production [`ChatAuthoringAccess`] impl the
//!     delegation listener calls when a chat agent asks codeg to save an
//!     automation or queue a board task. It resolves the target project, builds
//!     the same drafts the editors build, and writes through
//!     `automation_create_core` / `work_task_create_core` so the lists get their
//!     broadcasts and the work-task pump its nudge.
//!   * The `chat_authoring.*` settings knobs (**default false, both**) — read at
//!     MCP injection time via [`ChatAuthoringRuntimeConfig`] to build
//!     `--features`, and AGAIN at write time by [`DbChatAuthoring`] so turning
//!     the switch off takes effect on sessions that are already running.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};

use crate::acp::chat_authoring::{
    local_timezone, AuthoringContext, AuthoringOutcome, ChatAuthoringAccess, ChatAuthoringConfig,
    ChatAuthoringRuntimeConfig, ListWorkTasksSpec, NewAutomationSpec, NewWorkTaskSpec,
    SplitWorkTaskSpec, WorkTaskIdsSpec, WorkTaskToolCall, WorkTaskToolOutcome, WorkTaskToolResult,
    WorkTaskToolTask, LIST_WAIT_POLL_INTERVAL_MS, MAX_LIST_WAIT_MS, MAX_TOOL_TASK_IDS,
};
use crate::acp::types::PromptInputBlock;
use crate::app_error::AppCommandError;
use crate::db::entities::automation::{IsolationMode, TriggerKind};
use crate::db::entities::folder::FolderKind;
use crate::db::entities::work_task::WorkTaskStatus;
use crate::db::service::{
    app_metadata_service, conversation_service, folder_service, work_task_service,
};
use crate::db::AppDatabase;
use crate::models::agent::AgentType;
use crate::models::{
    AutomationConfig, AutomationDraft, FolderDetail, WorkTaskConfig, WorkTaskDraft, WorkTaskInfo,
};
use crate::web::event_bridge::{
    emit_event, EventEmitter, WorkTaskChange, CHAT_AUTHORING_SETTINGS_CHANGED_EVENT,
    WORK_TASK_CHANGED_EVENT,
};

const KIND_AUTOMATION: &str = "automation";
const KIND_WORK_TASK: &str = "work_task";

/// Production [`ChatAuthoringAccess`]. Holds the DB plus the event emitter (so
/// creates broadcast exactly like a UI-driven create) and the runtime config it
/// re-checks before every write.
pub struct DbChatAuthoring {
    pub db: Arc<AppDatabase>,
    pub emitter: EventEmitter,
    pub config: ChatAuthoringRuntimeConfig,
}

impl DbChatAuthoring {
    pub fn new(
        db: Arc<AppDatabase>,
        emitter: EventEmitter,
        config: ChatAuthoringRuntimeConfig,
    ) -> Self {
        Self {
            db,
            emitter,
            config,
        }
    }

    /// Resolve the project the new automation / task belongs to.
    ///
    /// Order: an explicit `folder_path` wins; otherwise the caller's own
    /// conversation; otherwise the working directory the companion was launched
    /// with. Whatever matches is then normalized to its project root — a
    /// worktree folder resolves to the project it was cut from, mirroring the
    /// "turn this message into a task" action in the UI.
    async fn resolve_folder(
        &self,
        ctx: &AuthoringContext,
        folder_path: Option<&str>,
    ) -> Result<FolderDetail, String> {
        let conn = &self.db.conn;
        let folders = folder_service::list_all_folder_details(conn)
            .await
            .map_err(|e| format!("could not read the folder list: {e}"))?;

        let found = if let Some(raw) = folder_path {
            match match_folder_by_path(&folders, raw) {
                Some(f) => f,
                None => {
                    // "known to codeg" — the lookup spans every non-deleted
                    // folder row, which includes projects the user has opened
                    // before but currently has closed.
                    return Err(format!(
                        "no project matching '{raw}' is known to codeg. Open the project first, \
                         or omit folder_path to use the one this conversation is in."
                    ));
                }
            }
        } else {
            let from_conversation = match ctx.conversation_id {
                Some(id) => conversation_service::get_by_id(conn, id)
                    .await
                    .ok()
                    .and_then(|c| folders.iter().find(|f| f.id == c.folder_id).cloned()),
                None => None,
            };
            match from_conversation
                .or_else(|| match_folder_by_path(&folders, &ctx.working_dir.to_string_lossy()))
            {
                Some(f) => f,
                None => {
                    return Err(
                        "could not tell which project this conversation belongs to. \
                                Pass folder_path with the absolute path of the target project."
                            .to_string(),
                    );
                }
            }
        };

        // Worktree folders are flattened (a worktree of a worktree still points
        // at the original root), so one hop is always enough.
        let root = match found.parent_id {
            Some(parent_id) => folders
                .iter()
                .find(|f| f.id == parent_id)
                .cloned()
                .unwrap_or(found),
            None => found,
        };
        if root.kind != FolderKind::Regular || root.parent_id.is_some() {
            return Err(format!(
                "'{}' is not a project folder that can hold automations or tasks. \
                 Pass folder_path with the absolute path of the target project.",
                root.path
            ));
        }
        Ok(root)
    }
}

/// Match `raw` against the registered folders: an exact path first, then the
/// longest folder the path lives inside. Component-wise (`Path::starts_with`),
/// so `/repo/app-2` never matches `/repo/app`.
fn match_folder_by_path(folders: &[FolderDetail], raw: &str) -> Option<FolderDetail> {
    let candidate = Path::new(raw.trim());
    if candidate.as_os_str().is_empty() {
        return None;
    }
    folders
        .iter()
        .filter(|f| candidate.starts_with(Path::new(&f.path)))
        // Deepest match wins: a conversation inside a worktree should resolve to
        // that worktree (and then walk up to its project), not straight to some
        // ancestor project that also contains it.
        .max_by_key(|f| Path::new(&f.path).components().count())
        .cloned()
}

/// Validate an agent wire slug (`claude_code`, `custom:<id>`, …). `None` input
/// stays `None` — the caller decides what to fall back to.
fn parse_agent_slug(raw: Option<&str>) -> Result<Option<AgentType>, String> {
    match raw {
        None => Ok(None),
        Some(s) => AgentType::from_wire(s)
            .map(Some)
            .ok_or_else(|| format!("unknown agent_type '{s}'")),
    }
}

/// Wrap a plain prompt string as the single text block both editors produce for
/// a text-only prompt.
fn text_prompt_blocks(prompt: &str) -> Result<Vec<serde_json::Value>, String> {
    let block = PromptInputBlock::Text {
        text: prompt.to_string(),
    };
    Ok(vec![serde_json::to_value(&block).map_err(|e| {
        format!("could not encode the prompt: {e}")
    })?])
}

#[async_trait]
impl ChatAuthoringAccess for DbChatAuthoring {
    async fn create_automation(
        &self,
        ctx: AuthoringContext,
        spec: NewAutomationSpec,
    ) -> AuthoringOutcome {
        // Re-check at call time, not just at injection: a session launched while
        // the switch was on keeps the tool listed after the user turns it off,
        // and "off" has to stop the write.
        if !self.config.automations_enabled().await {
            return AuthoringOutcome::rejected(
                KIND_AUTOMATION,
                "Creating automations from chat is turned off in codeg's settings \
                 (Settings → General → Create from chat). Ask the user to enable it.",
            );
        }
        let folder = match self.resolve_folder(&ctx, spec.folder_path.as_deref()).await {
            Ok(f) => f,
            Err(note) => return AuthoringOutcome::rejected(KIND_AUTOMATION, note),
        };
        let agent = match parse_agent_slug(spec.agent_type.as_deref()) {
            Ok(a) => a,
            Err(note) => return AuthoringOutcome::rejected(KIND_AUTOMATION, note),
        };
        // An automation always runs as a concrete agent (the fire path parses
        // this slug), so resolve a default rather than storing an empty string:
        // the project's configured agent, else whatever the caller is running as.
        let agent = match agent.or(folder.default_agent_type) {
            Some(a) => a,
            None => {
                let from_caller = match ctx.conversation_id {
                    Some(id) => conversation_service::get_by_id(&self.db.conn, id)
                        .await
                        .ok()
                        .map(|c| c.agent_type),
                    None => None,
                };
                match from_caller {
                    Some(a) => a,
                    None => {
                        return AuthoringOutcome::rejected(
                            KIND_AUTOMATION,
                            "this project has no default agent — pass agent_type \
                             (e.g. 'claude_code').",
                        );
                    }
                }
            }
        };
        let prompt_blocks = match text_prompt_blocks(&spec.prompt) {
            Ok(b) => b,
            Err(note) => return AuthoringOutcome::rejected(KIND_AUTOMATION, note),
        };
        let config = AutomationConfig {
            action: spec.action,
            prompt_blocks,
            display_text: spec.prompt.clone(),
            mode_id: None,
            config_values: BTreeMap::new(),
            label_snapshot: None,
        };
        let config = match serde_json::to_value(&config) {
            Ok(v) => v,
            Err(e) => {
                return AuthoringOutcome::rejected(
                    KIND_AUTOMATION,
                    format!("could not encode the automation config: {e}"),
                );
            }
        };
        let cron = spec.cron.clone();
        let timezone = spec.timezone.clone().unwrap_or_else(local_timezone);
        let draft = AutomationDraft {
            name: spec.name.clone(),
            enabled: spec.enabled,
            trigger_kind: if cron.is_some() {
                TriggerKind::Schedule
            } else {
                TriggerKind::Manual
            },
            cron: cron.clone(),
            timezone: timezone.clone(),
            agent_type: agent.as_wire().into_owned(),
            root_folder_id: Some(folder.id),
            // A fresh worktree per run is the safe default and the only shape
            // `enqueue_task` accepts; branch / shared-in-root stay an editor-only
            // choice.
            isolation: IsolationMode::WorktreePerRun,
            branch: None,
            is_remote_branch: false,
            config,
        };
        match crate::commands::automation::automation_create_core(&self.emitter, &self.db, draft)
            .await
        {
            Ok(info) => AuthoringOutcome {
                created: true,
                kind: KIND_AUTOMATION.to_string(),
                id: Some(info.id),
                title: Some(info.name),
                folder_name: Some(folder.name),
                folder_path: Some(folder.path),
                agent_type: Some(info.agent_type),
                cron: info.cron,
                timezone: Some(info.timezone),
                next_run_at: info.next_run_at,
                note: (!info.enabled).then(|| {
                    "Saved switched off — the user can enable it on the Automations page."
                        .to_string()
                }),
            },
            // The service's validation errors (bad cron, unknown timezone, empty
            // prompt) are exactly what the LLM should read and retry against, so
            // they come back as a soft note rather than a tool error.
            Err(e) => AuthoringOutcome::rejected(KIND_AUTOMATION, e.to_string()),
        }
    }

    async fn create_work_task(
        &self,
        ctx: AuthoringContext,
        spec: NewWorkTaskSpec,
    ) -> AuthoringOutcome {
        if !self.config.work_tasks_enabled().await {
            return AuthoringOutcome::rejected(
                KIND_WORK_TASK,
                "Creating board tasks from chat is turned off in codeg's settings \
                 (Settings → General → Create from chat). Ask the user to enable it.",
            );
        }
        let folder = match self.resolve_folder(&ctx, spec.folder_path.as_deref()).await {
            Ok(f) => f,
            Err(note) => return AuthoringOutcome::rejected(KIND_WORK_TASK, note),
        };
        let agent = match parse_agent_slug(spec.agent_type.as_deref()) {
            Ok(a) => a,
            Err(note) => return AuthoringOutcome::rejected(KIND_WORK_TASK, note),
        };
        let prompt_blocks = match text_prompt_blocks(&spec.prompt) {
            Ok(b) => b,
            Err(note) => return AuthoringOutcome::rejected(KIND_WORK_TASK, note),
        };
        // `agent_type: None` deliberately stays None — that is "inherit the
        // board's settings", which is what the user configured for this project.
        let config = WorkTaskConfig {
            prompt_blocks,
            display_text: spec.prompt.clone(),
            agent_type: agent.map(|a| a.as_wire().into_owned()),
            mode_id: None,
            config_values: BTreeMap::new(),
            label_snapshot: None,
            deliverable: None,
            // No branch face on the authoring tool: the task branches from the
            // project folder's checkout, as it did before the choice existed.
            base_branch: None,
        };
        let config = match serde_json::to_value(&config) {
            Ok(v) => v,
            Err(e) => {
                return AuthoringOutcome::rejected(
                    KIND_WORK_TASK,
                    format!("could not encode the task config: {e}"),
                );
            }
        };
        let draft = WorkTaskDraft {
            folder_id: folder.id,
            title: spec.title.clone(),
            config,
        };
        // The authoring path is the ONE create that stamps the caller's
        // conversation: that stamp is what later authorizes `start_work_task` /
        // `cancel_work_task` / `split_work_task` on this task.
        match crate::commands::work_task::work_task_create_authored_core(
            &self.emitter,
            &self.db,
            draft,
            ctx.conversation_id,
        )
        .await
        {
            Ok(info) => AuthoringOutcome {
                created: true,
                kind: KIND_WORK_TASK.to_string(),
                id: Some(info.id),
                title: Some(info.title),
                folder_name: Some(folder.name),
                folder_path: Some(folder.path),
                agent_type: spec.agent_type.clone(),
                note: Some(
                    "Queued as a to-do; the user starts it from there \
                     (or auto-processing picks it up)."
                        .to_string(),
                ),
                ..Default::default()
            },
            Err(e) => AuthoringOutcome::rejected(KIND_WORK_TASK, e.to_string()),
        }
    }

    /// The four orchestration tools, behind one entry point (see the trait
    /// docs). The switch is re-checked HERE, not only at MCP injection time:
    /// these tools WRITE board state and start agents, and "off" has to mean off
    /// for a session that was launched while it was on.
    async fn work_task_tool(
        &self,
        ctx: AuthoringContext,
        call: WorkTaskToolCall,
    ) -> WorkTaskToolOutcome {
        if !self.config.work_tasks_enabled().await {
            return WorkTaskToolOutcome::refused(refusal_feature_off());
        }
        // Identity is the whole authorization model of these tools: a task is
        // the caller's if this conversation created it, or created its parent.
        // Without a conversation there is nothing to match against — a
        // conversation-less caller owns nothing.
        let Some(conversation_id) = ctx.conversation_id else {
            return WorkTaskToolOutcome::refused(
                "This chat has no codeg session yet, so it has no board tasks to manage.",
            );
        };
        match call {
            WorkTaskToolCall::Split(spec) => self.split_work_task(conversation_id, spec).await,
            WorkTaskToolCall::List(spec) => self.list_work_tasks(conversation_id, spec).await,
            WorkTaskToolCall::Start(spec) => self.start_work_tasks(conversation_id, spec).await,
            WorkTaskToolCall::Cancel(spec) => self.cancel_work_tasks(conversation_id, spec).await,
        }
    }
}

/// The refusal shown when the `taskboard` switch is off. Same wording the
/// create tool uses, so the two always tell the user the same thing.
fn refusal_feature_off() -> String {
    "Managing board tasks from chat is turned off in codeg's settings \
     (Settings → General → Create from chat). Ask the user to enable it."
        .to_string()
}

impl DbChatAuthoring {
    /// The task, only if this conversation owns it. `None` covers "no such
    /// task", "deleted", and "someone else's" alike — callers answer with
    /// [`WorkTaskToolOutcome::not_yours`], which cannot confirm existence.
    async fn owned_task(
        &self,
        task_id: i32,
        conversation_id: i32,
    ) -> Option<crate::db::entities::work_task::Model> {
        let task = work_task_service::get_model(&self.db.conn, task_id)
            .await
            .ok()?;
        work_task_service::is_owned_by_conversation(
            &self.db.conn,
            &task,
            Some(conversation_id),
        )
        .await
        .ok()
        .filter(|owned| *owned)
        .map(|_| task)
    }

    /// `split_work_task`: create the subtasks under an owned parent, wire the
    /// intra-call / extra dependencies, write the limits, and (optionally) make
    /// the parent wait for its children.
    async fn split_work_task(
        &self,
        conversation_id: i32,
        spec: SplitWorkTaskSpec,
    ) -> WorkTaskToolOutcome {
        if spec.subtasks.is_empty() {
            return WorkTaskToolOutcome::refused("A split needs at least one subtask.");
        }
        let Some(parent) = self.owned_task(spec.parent_task_id, conversation_id).await else {
            return WorkTaskToolOutcome::not_yours(spec.parent_task_id);
        };
        if parent.parent_id.is_some() {
            return WorkTaskToolOutcome::refused(format!(
                "Task #{} is already a subtask; the hierarchy stops at two levels. Split the \
                 top-level task instead.",
                parent.id
            ));
        }

        let mut children = Vec::with_capacity(spec.subtasks.len());
        for (idx, sub) in spec.subtasks.iter().enumerate() {
            let agent = match parse_agent_slug(sub.agent_type.as_deref()) {
                Ok(a) => a,
                Err(note) => {
                    return WorkTaskToolOutcome::refused(format!("subtasks[{idx}]: {note}"))
                }
            };
            let prompt_blocks = match text_prompt_blocks(&sub.prompt) {
                Ok(b) => b,
                Err(note) => {
                    return WorkTaskToolOutcome::refused(format!("subtasks[{idx}]: {note}"))
                }
            };
            let config = WorkTaskConfig {
                prompt_blocks,
                display_text: sub.prompt.clone(),
                agent_type: agent.map(|a| a.as_wire().into_owned()),
                mode_id: None,
                config_values: BTreeMap::new(),
                label_snapshot: None,
                deliverable: None,
                base_branch: None,
            };
            let config = match serde_json::to_value(&config) {
                Ok(v) => v,
                Err(e) => {
                    return WorkTaskToolOutcome::refused(format!(
                        "could not encode subtask {idx} config: {e}"
                    ))
                }
            };
            children.push(work_task_service::WorkTaskChildDraft {
                title: sub.title.clone(),
                config,
                depends_on_index: sub.depends_on_index.clone(),
            });
        }

        let request = work_task_service::SplitTaskRequest {
            parent_id: parent.id,
            children,
            depends_on_task_ids: spec.depends_on_task_ids.clone(),
            max_concurrent_children: spec.limits.max_concurrent_children,
            max_runs_per_child: spec.limits.max_runs_per_child,
            token_budget: spec.limits.token_budget,
            parent_depends_on_children: spec.parent_depends_on_children,
            created_by_conversation_id: Some(conversation_id),
        };
        let outcome = match work_task_service::split_task(&self.db.conn, request).await {
            Ok(o) => o,
            // Semantic refusals (cycle, cross-folder dependency, bad limit) and
            // DB errors both come back as a note the LLM can act on.
            Err(e) => return WorkTaskToolOutcome::refused(e.to_string()),
        };
        for child in &outcome.children {
            emit_event(
                &self.emitter,
                WORK_TASK_CHANGED_EVENT,
                WorkTaskChange::Upsert { id: child.id },
            );
        }
        // The board (and an auto_process folder) should see the new to-dos now.
        crate::commands::work_task::nudge_pump(parent.folder_id);

        // The children's own derived state is part of the answer: a subtask that
        // waits on a sibling is `blocked`, and the caller asked for the split —
        // it should be told which pieces are queued behind which.
        let mut infos = outcome.children;
        if let Err(e) = work_task_service::annotate_blocked(&self.db.conn, &mut infos).await {
            tracing::warn!("[chat_authoring] could not annotate split children: {e}");
        }
        let mut note = format!(
            "Created {} subtask(s) as to-dos; nothing starts until they are started (or the \
             project auto-processes them).",
            infos.len()
        );
        if spec.parent_depends_on_children {
            note.push_str(
                " The parent now waits for all of them, so starting it means integrating the \
                 pieces.",
            );
        }
        WorkTaskToolOutcome {
            ok: true,
            tasks: infos.iter().map(to_tool_task).collect(),
            results: Vec::new(),
            note: Some(note),
        }
    }

    /// Resolve the window `list_work_tasks` reports on, and annotate it with
    /// the derived `blocked` state. The window's IDS are fixed by the request,
    /// but its membership can change while a long-poll waits (a subtask created
    /// by another call, a task deleted) — recomputed on every poll, which is
    /// exactly what makes the wait wake on that too.
    async fn work_task_window(
        &self,
        conversation_id: i32,
        spec: &ListWorkTasksSpec,
    ) -> Result<(Vec<WorkTaskInfo>, Option<String>), String> {
        let (mut infos, note) = if !spec.task_ids.is_empty() {
            let mut ids = Vec::with_capacity(spec.task_ids.len());
            let mut foreign = 0usize;
            for id in &spec.task_ids {
                if self.owned_task(*id, conversation_id).await.is_some() {
                    ids.push(*id);
                } else {
                    foreign += 1;
                }
            }
            let note = (foreign > 0).then(|| {
                format!(
                    "{foreign} requested task id(s) are not this conversation's and were left out."
                )
            });
            (
                work_task_service::list_by_ids(&self.db.conn, &ids)
                    .await
                    .map_err(|e| e.to_string())?,
                note,
            )
        } else if let Some(parent_id) = spec.parent_task_id {
            if self.owned_task(parent_id, conversation_id).await.is_none() {
                return Err(format!(
                    "Task #{parent_id} is not one of this conversation's tasks, so it cannot be \
                     read from here."
                ));
            }
            let mut infos = work_task_service::list_by_ids(&self.db.conn, &[parent_id])
                .await
                .map_err(|e| e.to_string())?;
            infos.extend(
                work_task_service::children_of(&self.db.conn, parent_id)
                    .await
                    .map_err(|e| e.to_string())?,
            );
            (infos, None)
        } else {
            (
                work_task_service::list_created_by_conversation(&self.db.conn, conversation_id)
                    .await
                    .map_err(|e| e.to_string())?,
                None,
            )
        };
        work_task_service::annotate_blocked(&self.db.conn, &mut infos)
            .await
            .map_err(|e| e.to_string())?;
        Ok((infos, note))
    }

    /// `list_work_tasks`: the caller's tasks (or one parent's children), with an
    /// optional bounded long-poll.
    async fn list_work_tasks(
        &self,
        conversation_id: i32,
        spec: ListWorkTasksSpec,
    ) -> WorkTaskToolOutcome {
        if spec.task_ids.len() > MAX_TOOL_TASK_IDS {
            return WorkTaskToolOutcome::refused(format!(
                "list_work_tasks names at most {MAX_TOOL_TASK_IDS} tasks at once."
            ));
        }
        let (mut infos, note) = match self.work_task_window(conversation_id, &spec).await {
            Ok(v) => v,
            Err(note) => return WorkTaskToolOutcome::refused(note),
        };
        let wait_ms = spec.wait_ms.unwrap_or(0).min(MAX_LIST_WAIT_MS);
        // Only wait when something could still change: a window that is empty
        // or entirely terminal has no next status to wake on.
        let can_change = infos.iter().any(|t| !is_terminal(t.status));
        if wait_ms > 0 && can_change {
            infos = self
                .await_status_change(conversation_id, &spec, infos, wait_ms)
                .await;
        }
        WorkTaskToolOutcome {
            ok: true,
            tasks: infos.iter().map(to_tool_task).collect(),
            results: Vec::new(),
            note,
        }
    }

    /// The polling half of the `list_work_tasks` long-poll: sleep ~1s at a time
    /// until any listed task's status differs from the snapshot we are about to
    /// return, or the cap runs out. Returns the freshest window either way.
    async fn await_status_change(
        &self,
        conversation_id: i32,
        spec: &ListWorkTasksSpec,
        mut infos: Vec<WorkTaskInfo>,
        wait_ms: u64,
    ) -> Vec<WorkTaskInfo> {
        let baseline = status_signature(&infos);
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return infos;
            }
            tokio::time::sleep(
                remaining.min(std::time::Duration::from_millis(LIST_WAIT_POLL_INTERVAL_MS)),
            )
            .await;
            match self.work_task_window(conversation_id, spec).await {
                Ok((latest, _)) => {
                    let changed = status_signature(&latest) != baseline;
                    infos = latest;
                    if changed {
                        return infos;
                    }
                }
                // A read that fails mid-wait returns what we already have; the
                // caller can ask again.
                Err(e) => {
                    tracing::warn!("[chat_authoring] list wait poll failed: {e}");
                    return infos;
                }
            }
        }
    }

    /// `start_work_task`: `todo → queued` through the engine's claim path, so
    /// every gate (dependencies, the parent's limits, the folder's concurrency,
    /// the folder's preflight) applies exactly as it does for the Start button.
    async fn start_work_tasks(
        &self,
        conversation_id: i32,
        spec: WorkTaskIdsSpec,
    ) -> WorkTaskToolOutcome {
        if spec.task_ids.len() > MAX_TOOL_TASK_IDS {
            return WorkTaskToolOutcome::refused(format!(
                "start_work_task names at most {MAX_TOOL_TASK_IDS} tasks at once."
            ));
        }
        let mut results = Vec::with_capacity(spec.task_ids.len());
        for task_id in &spec.task_ids {
            let Some(task) = self.owned_task(*task_id, conversation_id).await else {
                results.push(refused_result(*task_id, "not one of this conversation's tasks"));
                continue;
            };
            if task.status != WorkTaskStatus::Todo {
                results.push(refused_result(
                    *task_id,
                    &format!(
                        "it is {} — only a to-do can be started",
                        work_task_service::status_str(task.status)
                    ),
                ));
                continue;
            }
            // Report the gate rather than letting the claim lose silently.
            match work_task_service::blocked_for(&self.db.conn, &task).await {
                Ok(Some(blocked)) => {
                    results.push(refused_result(
                        *task_id,
                        &format!(
                            "it cannot start yet: {}",
                            work_task_service::blocked_message(&blocked)
                        ),
                    ));
                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    results.push(refused_result(*task_id, &e.to_string()));
                    continue;
                }
            }
            match self.claim_start(*task_id, task.folder_id).await {
                Ok(()) => results.push(WorkTaskToolResult {
                    task_id: *task_id,
                    outcome: "ok".to_string(),
                    status: Some("queued".to_string()),
                    note: None,
                }),
                Err(note) => results.push(refused_result(*task_id, &note)),
            }
        }
        WorkTaskToolOutcome {
            ok: true,
            tasks: Vec::new(),
            results,
            note: None,
        }
    }

    /// One start, through the engine when this process owns one (preflight +
    /// pump + broadcast), else through the plain claim (the process that owns
    /// the engine picks the row up from its tick).
    async fn claim_start(&self, task_id: i32, folder_id: i32) -> Result<(), String> {
        if let Some(engine) = crate::work_task::engine() {
            return engine.start(task_id).await;
        }
        match work_task_service::claim_for_run(&self.db.conn, task_id, WorkTaskStatus::Todo, "agent")
            .await
            .map_err(|e| e.to_string())?
        {
            Some(_) => {
                emit_event(
                    &self.emitter,
                    WORK_TASK_CHANGED_EVENT,
                    WorkTaskChange::Upsert { id: task_id },
                );
                crate::commands::work_task::nudge_pump(folder_id);
                Ok(())
            }
            None => Err("the task could not be claimed (it changed state first)".to_string()),
        }
    }

    /// `cancel_work_task`: stop an owned task. Terminal tasks and a merge in
    /// flight are refusals, never tool errors — the caller reads them and tells
    /// the user.
    async fn cancel_work_tasks(
        &self,
        conversation_id: i32,
        spec: WorkTaskIdsSpec,
    ) -> WorkTaskToolOutcome {
        if spec.task_ids.len() > MAX_TOOL_TASK_IDS {
            return WorkTaskToolOutcome::refused(format!(
                "cancel_work_task names at most {MAX_TOOL_TASK_IDS} tasks at once."
            ));
        }
        let mut results = Vec::with_capacity(spec.task_ids.len());
        for task_id in &spec.task_ids {
            let Some(task) = self.owned_task(*task_id, conversation_id).await else {
                results.push(refused_result(*task_id, "not one of this conversation's tasks"));
                continue;
            };
            if is_terminal(task.status) {
                results.push(refused_result(
                    *task_id,
                    &format!(
                        "it already finished ({}) — requeue the card from the board to run it \
                         again",
                        work_task_service::status_str(task.status)
                    ),
                ));
                continue;
            }
            if task.status == WorkTaskStatus::Merging {
                results.push(refused_result(
                    *task_id,
                    "its merge is in flight — a merge cannot be stopped once it starts",
                ));
                continue;
            }
            match self.claim_cancel(*task_id, task.folder_id).await {
                Ok(()) => results.push(WorkTaskToolResult {
                    task_id: *task_id,
                    outcome: "ok".to_string(),
                    status: Some("canceled".to_string()),
                    note: None,
                }),
                Err(note) => results.push(refused_result(*task_id, &note)),
            }
        }
        WorkTaskToolOutcome {
            ok: true,
            tasks: Vec::new(),
            results,
            note: None,
        }
    }

    /// One cancel: the engine's path sheds the live connection and pumps the
    /// folder; without an engine the plain service cancel is all there is.
    async fn claim_cancel(&self, task_id: i32, folder_id: i32) -> Result<(), String> {
        if let Some(engine) = crate::work_task::engine() {
            return engine.cancel(task_id, None).await;
        }
        let canceled = work_task_service::cancel(&self.db.conn, task_id, None)
            .await
            .map_err(|e| e.to_string())?;
        if canceled {
            emit_event(
                &self.emitter,
                WORK_TASK_CHANGED_EVENT,
                WorkTaskChange::Upsert { id: task_id },
            );
            crate::commands::work_task::nudge_pump(folder_id);
            Ok(())
        } else {
            Err("the task could not be canceled in its current state".to_string())
        }
    }
}

fn refused_result(task_id: i32, note: &str) -> WorkTaskToolResult {
    WorkTaskToolResult {
        task_id,
        outcome: "refused".to_string(),
        status: None,
        note: Some(note.to_string()),
    }
}

/// `done` / `failed` / `canceled` — no claim or cancel left to make.
fn is_terminal(status: WorkTaskStatus) -> bool {
    matches!(
        status,
        WorkTaskStatus::Done | WorkTaskStatus::Failed | WorkTaskStatus::Canceled
    )
}

/// The `(id, status)` pairs a long-poll compares between passes.
fn status_signature(infos: &[WorkTaskInfo]) -> Vec<(i32, WorkTaskStatus)> {
    let mut out: Vec<(i32, WorkTaskStatus)> = infos.iter().map(|t| (t.id, t.status)).collect();
    out.sort_unstable_by_key(|(id, _)| *id);
    out
}

/// Trim a row to the view the orchestration tools answer with.
fn to_tool_task(info: &WorkTaskInfo) -> WorkTaskToolTask {
    WorkTaskToolTask {
        id: info.id,
        title: info.title.clone(),
        status: work_task_service::status_str(info.status).to_string(),
        run_seq: info.run_seq,
        parent_id: info.parent_id,
        latest_progress: info.latest_progress.clone(),
        verdict: info.verdict.clone(),
        failure_reason: info.failure_reason.clone(),
        blocked: info.blocked.clone(),
        files_changed: info.files_changed,
        additions: info.additions,
        deletions: info.deletions,
    }
}

// ===========================================================================
// Settings persistence — `chat_authoring.automations_enabled` /
// `chat_authoring.work_tasks_enabled` (both default OFF). Mirrors
// `crate::commands::session_info`.
// ===========================================================================

pub const KEY_CHAT_AUTHORING_AUTOMATIONS: &str = "chat_authoring.automations_enabled";
pub const KEY_CHAT_AUTHORING_WORK_TASKS: &str = "chat_authoring.work_tasks_enabled";

/// Off by default, unlike the read-only `get_session_info` / `ask_user_question`
/// toggles: these tools write app state and a scheduled automation goes on to
/// spawn agents unattended, so the user opts in.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatAuthoringSettings {
    pub automations_enabled: bool,
    pub work_tasks_enabled: bool,
}

impl ChatAuthoringSettings {
    fn into_runtime_config(self) -> ChatAuthoringConfig {
        ChatAuthoringConfig {
            automations_enabled: self.automations_enabled,
            work_tasks_enabled: self.work_tasks_enabled,
        }
    }
}

async fn load_flag(conn: &DatabaseConnection, key: &str) -> bool {
    match app_metadata_service::get_value(conn, key).await {
        Ok(Some(raw)) => raw.parse::<bool>().unwrap_or(false),
        _ => false,
    }
}

/// Read the persisted keys from `app_metadata`, falling back to the default
/// (both off) for a missing or malformed value. Never errors hard.
pub async fn load_chat_authoring_settings(conn: &DatabaseConnection) -> ChatAuthoringSettings {
    ChatAuthoringSettings {
        automations_enabled: load_flag(conn, KEY_CHAT_AUTHORING_AUTOMATIONS).await,
        work_tasks_enabled: load_flag(conn, KEY_CHAT_AUTHORING_WORK_TASKS).await,
    }
}

/// Pull settings from the DB and push the resulting [`ChatAuthoringConfig`] onto
/// the shared runtime handle. Idempotent — safe on startup or after any save.
pub async fn apply_persisted_chat_authoring_config(
    conn: &DatabaseConnection,
    config: &ChatAuthoringRuntimeConfig,
) {
    let settings = load_chat_authoring_settings(conn).await;
    config.set(settings.into_runtime_config()).await;
}

/// Serializes every write to this record within the process.
///
/// Per-key upserts alone are not enough. Both writers below finish by re-reading
/// the record and pushing it onto the runtime config, and `load_*` reads the two
/// keys in two separate queries — so two writers can interleave such that the
/// database ends correct while the runtime handle (which is what the companion
/// injection actually reads) settles on a value neither writer intended, and
/// stays there until the next write or a restart. Holding this across
/// write → re-read → apply → broadcast makes the sequence atomic.
static AUTHORING_WRITE_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Which of the two independent switches in the record to move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatAuthoringFlag {
    Automations,
    WorkTasks,
}

/// Move exactly one switch, leaving the sibling to whatever the database says
/// at the moment of the write.
///
/// [`set_chat_authoring_settings_core`] republishes both keys, which is right
/// for the settings form (it edits both) and wrong for any caller that only
/// means to move one: two such callers racing — the status-bar popover's two
/// adjacent switches are one click apart — each read the pair, each write the
/// pair back, and whoever lands second silently reverts the other's flip. This
/// writes one key and then re-reads the record, so concurrent flips of the two
/// switches commute and the broadcast carries the record as it now stands
/// rather than as the caller imagined it.
pub async fn set_chat_authoring_flag_core(
    conn: &DatabaseConnection,
    config: &ChatAuthoringRuntimeConfig,
    emitter: &EventEmitter,
    flag: ChatAuthoringFlag,
    enabled: bool,
) -> Result<ChatAuthoringSettings, AppCommandError> {
    let key = match flag {
        ChatAuthoringFlag::Automations => KEY_CHAT_AUTHORING_AUTOMATIONS,
        ChatAuthoringFlag::WorkTasks => KEY_CHAT_AUTHORING_WORK_TASKS,
    };
    let _guard = AUTHORING_WRITE_LOCK.lock().await;
    app_metadata_service::upsert_value(conn, key, &enabled.to_string())
        .await
        .map_err(AppCommandError::from)?;
    let settings = load_chat_authoring_settings(conn).await;
    config.set(settings.clone().into_runtime_config()).await;
    emit_event(emitter, CHAT_AUTHORING_SETTINGS_CHANGED_EVENT, &settings);
    Ok(settings)
}

/// Persist + apply + broadcast. Shared by the Tauri command and the HTTP handler
/// so the write + re-apply + notify chain lives in one place.
///
/// Takes [`AUTHORING_WRITE_LOCK`] too: its two upserts are not atomic either, so
/// a per-flag write landing between them would be half-overwritten.
pub async fn set_chat_authoring_settings_core(
    conn: &DatabaseConnection,
    config: &ChatAuthoringRuntimeConfig,
    emitter: &EventEmitter,
    desired: ChatAuthoringSettings,
) -> Result<ChatAuthoringSettings, AppCommandError> {
    let _guard = AUTHORING_WRITE_LOCK.lock().await;
    app_metadata_service::upsert_value(
        conn,
        KEY_CHAT_AUTHORING_AUTOMATIONS,
        &desired.automations_enabled.to_string(),
    )
    .await
    .map_err(AppCommandError::from)?;
    app_metadata_service::upsert_value(
        conn,
        KEY_CHAT_AUTHORING_WORK_TASKS,
        &desired.work_tasks_enabled.to_string(),
    )
    .await
    .map_err(AppCommandError::from)?;
    config.set(desired.clone().into_runtime_config()).await;
    emit_event(emitter, CHAT_AUTHORING_SETTINGS_CHANGED_EVENT, &desired);
    Ok(desired)
}

// -------- Tauri commands -----------------------------------------------------

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn get_chat_authoring_settings(
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, crate::db::AppDatabase>,
) -> Result<ChatAuthoringSettings, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        Ok(load_chat_authoring_settings(&db.conn).await)
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn set_chat_authoring_settings(
    #[cfg(feature = "tauri-runtime")] app: tauri::AppHandle,
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, crate::db::AppDatabase>,
    #[cfg(feature = "tauri-runtime")] config: tauri::State<'_, ChatAuthoringRuntimeConfig>,
    settings: ChatAuthoringSettings,
) -> Result<ChatAuthoringSettings, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        let emitter = EventEmitter::Tauri(app);
        set_chat_authoring_settings_core(&db.conn, &config, &emitter, settings).await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = settings;
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn folder(id: i32, path: &str, parent: Option<i32>, kind: FolderKind) -> FolderDetail {
        FolderDetail {
            id,
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            path: path.to_string(),
            git_branch: None,
            default_agent_type: None,
            last_opened_at: Utc::now(),
            sort_order: 0,
            color: "blue".into(),
            parent_id: parent,
            kind,
            alias: None,
            group_id: None,
        }
    }

    #[test]
    fn match_folder_by_path_prefers_exact_then_deepest() {
        let folders = vec![
            folder(1, "/repo/app", None, FolderKind::Regular),
            folder(2, "/repo/app-2", None, FolderKind::Regular),
            folder(3, "/repo/app/worktrees/wt", Some(1), FolderKind::Regular),
        ];
        // Exact hit.
        assert_eq!(match_folder_by_path(&folders, "/repo/app").unwrap().id, 1);
        // A sibling whose name merely shares a prefix must NOT match.
        assert_eq!(match_folder_by_path(&folders, "/repo/app-2").unwrap().id, 2);
        // A path inside a folder resolves to the deepest containing folder.
        assert_eq!(
            match_folder_by_path(&folders, "/repo/app/worktrees/wt/src/lib.rs")
                .unwrap()
                .id,
            3
        );
        assert_eq!(
            match_folder_by_path(&folders, "/repo/app/src/lib.rs")
                .unwrap()
                .id,
            1
        );
        // Nothing registered under this path.
        assert!(match_folder_by_path(&folders, "/elsewhere").is_none());
        assert!(match_folder_by_path(&folders, "   ").is_none());
    }

    #[test]
    fn parse_agent_slug_validates() {
        assert_eq!(parse_agent_slug(None).unwrap(), None);
        assert_eq!(
            parse_agent_slug(Some("claude_code")).unwrap(),
            Some(AgentType::ClaudeCode)
        );
        assert!(parse_agent_slug(Some("not_an_agent")).is_err());
    }

    #[test]
    fn text_prompt_blocks_is_one_text_block() {
        let blocks = text_prompt_blocks("do the thing").unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "do the thing");
    }

    #[test]
    fn settings_default_is_both_off() {
        let s = ChatAuthoringSettings::default();
        assert!(!s.automations_enabled);
        assert!(!s.work_tasks_enabled);
    }

    // ── integration: the real write path against an in-memory DB ────────────

    use crate::db::service::{automation_service, work_task_service};
    use crate::db::test_helpers::fresh_in_memory_db;

    /// Wire a `DbChatAuthoring` over a fresh DB with both flags set as given.
    async fn harness(
        automations: bool,
        work_tasks: bool,
    ) -> (
        Arc<AppDatabase>,
        DbChatAuthoring,
        ChatAuthoringRuntimeConfig,
    ) {
        let db = Arc::new(fresh_in_memory_db().await);
        let config = ChatAuthoringRuntimeConfig::new();
        config
            .set(ChatAuthoringConfig {
                automations_enabled: automations,
                work_tasks_enabled: work_tasks,
            })
            .await;
        let access = DbChatAuthoring::new(db.clone(), EventEmitter::Noop, config.clone());
        (db, access, config)
    }

    fn ctx_at(dir: &str) -> AuthoringContext {
        AuthoringContext {
            conversation_id: None,
            working_dir: std::path::PathBuf::from(dir),
        }
    }

    fn automation_spec() -> NewAutomationSpec {
        NewAutomationSpec {
            name: "Nightly audit".into(),
            prompt: "audit the dependencies".into(),
            cron: Some("0 3 * * *".into()),
            timezone: Some("UTC".into()),
            action: Default::default(),
            agent_type: Some("claude_code".into()),
            folder_path: None,
            enabled: true,
        }
    }

    fn work_task_spec() -> NewWorkTaskSpec {
        NewWorkTaskSpec {
            title: "Fix the flake".into(),
            prompt: "the retry test is flaky".into(),
            agent_type: None,
            folder_path: None,
        }
    }

    /// The headline invariant: with the switch off the tool writes NOTHING, even
    /// though the companion may still be advertising it (it was injected while
    /// the switch was on). The injection-time check alone would let this through.
    #[tokio::test]
    async fn create_automation_refuses_and_writes_nothing_when_disabled() {
        let (db, access, _cfg) = harness(false, true).await;
        folder_service::add_folder(&db.conn, "/repo/app")
            .await
            .unwrap();

        let out = access
            .create_automation(ctx_at("/repo/app"), automation_spec())
            .await;

        assert!(!out.created);
        assert_eq!(out.kind, "automation");
        assert!(out.note.unwrap().contains("turned off"));
        assert!(automation_service::list(&db.conn).await.unwrap().is_empty());
    }

    /// …and the two flags gate independently: automations on must not unlock the
    /// board tool.
    #[tokio::test]
    async fn create_work_task_refuses_when_only_automations_enabled() {
        let (db, access, _cfg) = harness(true, false).await;
        folder_service::add_folder(&db.conn, "/repo/app")
            .await
            .unwrap();

        let out = access
            .create_work_task(ctx_at("/repo/app"), work_task_spec())
            .await;

        assert!(!out.created);
        assert!(out.note.unwrap().contains("turned off"));
        assert!(work_task_service::list(&db.conn, None)
            .await
            .unwrap()
            .is_empty());
    }

    /// Flipping the shared runtime handle takes effect on the NEXT call — no
    /// reconstruction of the access impl needed. This is what makes the call-time
    /// check meaningful for an already-running session.
    #[tokio::test]
    async fn flipping_the_flag_takes_effect_on_the_next_call() {
        let (db, access, cfg) = harness(false, false).await;
        folder_service::add_folder(&db.conn, "/repo/app")
            .await
            .unwrap();

        assert!(
            !access
                .create_automation(ctx_at("/repo/app"), automation_spec())
                .await
                .created
        );
        cfg.set(ChatAuthoringConfig {
            automations_enabled: true,
            work_tasks_enabled: false,
        })
        .await;
        let out = access
            .create_automation(ctx_at("/repo/app"), automation_spec())
            .await;
        assert!(out.created, "note: {:?}", out.note);
        assert_eq!(automation_service::list(&db.conn).await.unwrap().len(), 1);
    }

    /// A successful create lands a real row with the shape the fire path expects,
    /// and reports back the schedule the scheduler actually computed.
    #[tokio::test]
    async fn create_automation_persists_a_fireable_row() {
        let (db, access, _cfg) = harness(true, false).await;
        let folder = folder_service::add_folder(&db.conn, "/repo/app")
            .await
            .unwrap();

        let out = access
            .create_automation(ctx_at("/repo/app"), automation_spec())
            .await;
        assert!(out.created, "note: {:?}", out.note);
        assert_eq!(out.folder_path.as_deref(), Some("/repo/app"));
        assert!(
            out.next_run_at.is_some(),
            "a scheduled automation has a next run"
        );

        let row = automation_service::get(&db.conn, out.id.unwrap())
            .await
            .unwrap();
        assert_eq!(row.root_folder_id, Some(folder.id));
        assert_eq!(row.agent_type, "claude_code");
        assert_eq!(row.cron.as_deref(), Some("0 3 * * *"));
        assert_eq!(row.isolation, IsolationMode::WorktreePerRun);
        assert!(row.branch.is_none());
        // The fire path parses this slug and replays these blocks verbatim.
        let cfg: AutomationConfig = serde_json::from_value(row.config).unwrap();
        assert_eq!(cfg.prompt_blocks.len(), 1);
        assert_eq!(cfg.prompt_blocks[0]["text"], "audit the dependencies");
        assert_eq!(cfg.display_text, "audit the dependencies");
    }

    /// A chat running inside a worktree targets the PROJECT, not the worktree —
    /// the board and the automations list are both project-scoped.
    #[tokio::test]
    async fn create_work_task_resolves_a_worktree_to_its_project() {
        let (db, access, _cfg) = harness(false, true).await;
        let root = folder_service::add_folder(&db.conn, "/repo/app")
            .await
            .unwrap();
        folder_service::add_folder_with_parent(&db.conn, "/repo/app-task-1", Some(root.id))
            .await
            .unwrap();

        // Deep inside the worktree, so the deepest-match + parent-hop both run.
        let out = access
            .create_work_task(ctx_at("/repo/app-task-1/src"), work_task_spec())
            .await;

        assert!(out.created, "note: {:?}", out.note);
        let row = work_task_service::get(&db.conn, out.id.unwrap())
            .await
            .unwrap();
        assert_eq!(row.folder_id, root.id, "task belongs to the project root");
        // No agent override — the task inherits the board's configured default.
        let cfg: WorkTaskConfig = serde_json::from_value(row.config).unwrap();
        assert!(cfg.agent_type.is_none());
        assert_eq!(cfg.prompt_blocks.len(), 1);
    }

    /// An unresolvable target is a soft refusal telling the LLM what to pass —
    /// never a silent write to some arbitrary folder.
    #[tokio::test]
    async fn unresolvable_folder_is_a_soft_refusal() {
        let (db, access, _cfg) = harness(true, true).await;
        folder_service::add_folder(&db.conn, "/repo/app")
            .await
            .unwrap();

        // Working dir outside every registered folder.
        let out = access
            .create_automation(ctx_at("/somewhere/else"), automation_spec())
            .await;
        assert!(!out.created);
        assert!(out.note.unwrap().contains("folder_path"));

        // An explicit path that is not a registered folder.
        let mut spec = work_task_spec();
        spec.folder_path = Some("/not/registered".into());
        let out = access.create_work_task(ctx_at("/repo/app"), spec).await;
        assert!(!out.created);
        assert!(out.note.unwrap().contains("/not/registered"));
        assert!(work_task_service::list(&db.conn, None)
            .await
            .unwrap()
            .is_empty());
    }

    /// Settings persist and re-apply onto the shared runtime handle, so a save in
    /// one transport is visible to MCP injection and to the write path.
    #[tokio::test]
    async fn settings_persist_and_reapply_to_the_runtime_handle() {
        let db = fresh_in_memory_db().await;
        let config = ChatAuthoringRuntimeConfig::new();

        // Nothing stored yet → both off.
        apply_persisted_chat_authoring_config(&db.conn, &config).await;
        assert_eq!(config.snapshot().await, ChatAuthoringConfig::default());

        set_chat_authoring_settings_core(
            &db.conn,
            &config,
            &EventEmitter::Noop,
            ChatAuthoringSettings {
                automations_enabled: true,
                work_tasks_enabled: false,
            },
        )
        .await
        .unwrap();
        assert!(config.automations_enabled().await);
        assert!(!config.work_tasks_enabled().await);

        // Round-trips through the DB (what a fresh boot reads).
        let loaded = load_chat_authoring_settings(&db.conn).await;
        assert!(loaded.automations_enabled);
        assert!(!loaded.work_tasks_enabled);
        let fresh = ChatAuthoringRuntimeConfig::new();
        apply_persisted_chat_authoring_config(&db.conn, &fresh).await;
        assert!(fresh.automations_enabled().await);
        assert!(!fresh.work_tasks_enabled().await);
    }
}
