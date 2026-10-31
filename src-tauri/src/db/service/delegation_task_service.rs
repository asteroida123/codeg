//! Durable ordinary-delegation admission and terminal-result ledger.
//!
//! This module intentionally owns only the durable facts needed by the
//! delegation runtime. It does not own connections, turns, retries, or
//! process cleanup. The runtime performs its busy/strict-resume checks first,
//! then calls [`admit`] immediately before sending the child prompt.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, Condition, ConnectionTrait,
    DatabaseConnection, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder, QuerySelect, Set,
    TransactionTrait,
};

use crate::acp::delegation::types::{DelegationTaskReport, TaskStatus};
use crate::db::entities::{conversation, delegation_task, folder};
use crate::db::error::DbError;
use crate::db::service::token_usage_service;
use crate::models::AgentType;

/// The session identity and selector preferences captured for a child execution.
///
/// This is serialized into the ledger as one JSON value so a future resume can
/// validate agent/session/cwd identity and best-effort restore its selectors.
/// `working_dir` is the canonical directory actually used by the child, while
/// `requested_working_dir` on [`AdmissionInput`] preserves the caller's raw
/// request for retry correlation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResumeBinding {
    pub agent_type: AgentType,
    pub external_session_id: String,
    pub child_conversation_id: i32,
    pub working_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Effective value after the runtime applies its requested/default mode.
    /// The `preferred_` name matches the manager/spawner hand-off vocabulary.
    pub preferred_mode_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub preferred_config_values: BTreeMap<String, String>,
    /// Historical launch snapshot for diagnostics; configuration is not
    /// session identity and may legitimately change between continuations.
    pub config_fingerprint: String,
}

/// Prepared admission input. The runtime must supply the already-created
/// child row and the strict binding it intends to use; no prompt is sent by
/// this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionInput {
    pub task_id: String,
    pub parent_conversation_id: i32,
    pub child_conversation_id: i32,
    pub source_task_id: Option<String>,
    pub task: String,
    pub requested_working_dir: Option<String>,
    pub resume_binding: ResumeBinding,
    /// The work task whose execution produced this delegation, resolved by the
    /// runtime from the delegating parent conversation. Stored (not derived at
    /// read time): a later fresh-session rework repoints
    /// `work_task.conversation_id`, and this row must keep the task it
    /// actually ran for.
    pub work_task_id: Option<i32>,
}

/// Metadata plus the report visible to the broker. A terminal report is read
/// from the immutable JSON snapshot; a running row gets a synthesized running
/// report and never reads the child's mutable conversation status.
#[derive(Debug, Clone)]
pub struct TaskLedgerEntry {
    pub id: i32,
    pub task_id: String,
    pub parent_conversation_id: i32,
    pub child_conversation_id: i32,
    pub source_task_id: Option<String>,
    pub task: String,
    pub requested_working_dir: Option<String>,
    pub resume_binding: ResumeBinding,
    pub status: TaskStatus,
    pub released: bool,
    pub report: DelegationTaskReport,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Attribution to the executing work task (see
    /// [`AdmissionInput::work_task_id`]).
    pub work_task_id: Option<i32>,
}

/// Result of the atomic source-slot admission.
#[derive(Debug, Clone)]
pub enum AdmissionResult {
    New {
        entry: TaskLedgerEntry,
    },
    Existing {
        entry: TaskLedgerEntry,
    },
    Conflict {
        next_task_id: String,
        reason: String,
    },
}

/// Distinguishes a pre-ledger task from a ledger task hidden by parent or
/// soft-delete authorization. Callers may use legacy storage only for
/// `Absent`; `Hidden` must remain opaque.
#[derive(Debug, Clone)]
// The visible branch intentionally returns the complete immutable ledger
// snapshot; boxing every scoped read would add allocation to the hot status
// path solely to shrink the two marker variants.
#[allow(clippy::large_enum_variant)]
pub enum ScopedLookup {
    Visible(TaskLedgerEntry),
    Hidden,
    Absent,
}

/// Insert one durable execution record. A source slot is reserved by the
/// unique index on `source_task_id`; a loser of that race is reconciled with
/// the winner and never sends a second prompt for the same source.
pub async fn admit(
    conn: &DatabaseConnection,
    input: AdmissionInput,
) -> Result<AdmissionResult, DbError> {
    admit_on(conn, input).await
}

/// Reserve a continuation source slot and advance the child's live routing
/// pointer in one SQLite transaction. A crash can therefore expose neither a
/// phantom pointer nor an admitted task whose prompt cannot be routed.
pub async fn admit_continuation(
    conn: &DatabaseConnection,
    input: AdmissionInput,
) -> Result<AdmissionResult, DbError> {
    let source_task_id = input.source_task_id.clone().ok_or_else(|| {
        DbError::Validation("continuation admission requires a source task".into())
    })?;
    let txn = conn.begin().await?;
    let result = admit_on(&txn, input.clone()).await?;
    let next_task_id = match &result {
        AdmissionResult::New { entry } => &entry.task_id,
        AdmissionResult::Existing { .. } | AdmissionResult::Conflict { .. } => {
            txn.commit().await?;
            return Ok(result);
        }
    };
    let updated = conversation::Entity::update_many()
        .col_expr(
            conversation::Column::DelegationCallId,
            sea_orm::sea_query::Expr::value(next_task_id.clone()),
        )
        .filter(conversation::Column::Id.eq(input.child_conversation_id))
        .filter(
            Condition::any()
                .add(conversation::Column::DelegationCallId.eq(&source_task_id))
                .add(conversation::Column::DelegationCallId.is_null()),
        )
        .exec(&txn)
        .await?;
    if updated.rows_affected != 1 {
        return Err(DbError::Conflict(
            "child session was taken over during continuation admission".into(),
        ));
    }
    txn.commit().await?;
    Ok(result)
}

async fn admit_on<C: ConnectionTrait>(
    conn: &C,
    input: AdmissionInput,
) -> Result<AdmissionResult, DbError> {
    validate_input(&input)?;
    ensure_live_conversation(conn, input.parent_conversation_id, "parent").await?;
    ensure_live_conversation(conn, input.child_conversation_id, "child").await?;

    if let Some(source_task_id) = input.source_task_id.as_deref() {
        // Retries must resolve an already-admitted successor before checking
        // whether the source can be started again. This is important after a
        // process restart: the successor may still be running while the
        // caller repeats the same request.
        if let Some(winner) = delegation_task::Entity::find()
            .filter(delegation_task::Column::ParentConversationId.eq(input.parent_conversation_id))
            .filter(delegation_task::Column::SourceTaskId.eq(source_task_id))
            .one(conn)
            .await?
        {
            let Some(entry) =
                load_authorized(conn, input.parent_conversation_id, &winner.task_id).await?
            else {
                return Err(DbError::Conflict(format!(
                    "source task {source_task_id} is already reserved but is no longer queryable"
                )));
            };
            if same_admission_key(&winner, &input)? {
                return Ok(AdmissionResult::Existing { entry });
            }
            return Ok(AdmissionResult::Conflict {
                next_task_id: winner.task_id,
                reason: format!(
                    "source task {source_task_id} already has a successor with a different task, agent, or working directory"
                ),
            });
        }

        let source = find_raw_by_task_id(conn, source_task_id)
            .await?
            .ok_or_else(|| DbError::NotFound(format!("source task {source_task_id}")))?;
        if source.parent_conversation_id != input.parent_conversation_id {
            return Err(DbError::NotFound(format!("source task {source_task_id}")));
        }
        if !is_terminal_status(&source.status) || !source.released {
            return Err(DbError::Validation(format!(
                "source task {source_task_id} is not eligible: it must be terminal and released"
            )));
        }
        let source_binding: ResumeBinding =
            serde_json::from_str(&source.resume_binding).map_err(|e| {
                DbError::Migration(format!(
                    "invalid resume binding for source {source_task_id}: {e}"
                ))
            })?;
        if source_binding != input.resume_binding {
            return Err(DbError::Conflict(format!(
                "source task {source_task_id} binding does not match the requested child session"
            )));
        }
        // This also checks the source child and its folder. A retained source
        // is not a usable resume anchor after either conversation is deleted.
        if load_authorized(conn, input.parent_conversation_id, source_task_id)
            .await?
            .is_none()
        {
            return Err(DbError::NotFound(format!("source task {source_task_id}")));
        }
    }

    let binding_json = serde_json::to_string(&input.resume_binding)
        .map_err(|e| DbError::Validation(format!("invalid resume binding: {e}")))?;
    let now = Utc::now();
    let active = delegation_task::ActiveModel {
        id: NotSet,
        task_id: Set(input.task_id.clone()),
        parent_conversation_id: Set(input.parent_conversation_id),
        child_conversation_id: Set(input.child_conversation_id),
        source_task_id: Set(input.source_task_id.clone()),
        task: Set(input.task.clone()),
        requested_working_dir: Set(input.requested_working_dir.clone()),
        status: Set(status_string(TaskStatus::Running)),
        terminal_report: Set(None),
        resume_binding: Set(binding_json),
        released: Set(false),
        // Resolved by the runtime from the delegating parent conversation
        // (a work task's current conversation) and stored for stability.
        work_task_id: Set(input.work_task_id),
        created_at: Set(now),
        updated_at: Set(now),
        // Known at admission (from the binding), so still-running rows already
        // group by agent in the dashboard. The terminal columns stay NULL
        // until `finish` extracts them from the report in one statement.
        agent_type: Set(Some(input.resume_binding.agent_type.as_wire().to_string())),
        error_code: Set(None),
        duration_ms: Set(None),
        turn_count: Set(None),
        input_tokens: Set(None),
        output_tokens: Set(None),
        cache_creation_tokens: Set(None),
        cache_read_tokens: Set(None),
        effective_model: Set(None),
        effective_mode: Set(None),
        effective_reasoning_level: Set(None),
    };

    match active.insert(conn).await {
        Ok(model) => Ok(AdmissionResult::New {
            entry: entry_from_model(model)?,
        }),
        Err(insert_error) => reconcile_insert_race(conn, &input, insert_error).await,
    }
}

/// Lookup a task under parent authorization. Deleted parent/child/folder rows
/// are intentionally indistinguishable from an unknown task.
pub async fn lookup(
    conn: &DatabaseConnection,
    parent_conversation_id: i32,
    task_id: &str,
) -> Result<Option<TaskLedgerEntry>, DbError> {
    load_authorized(conn, parent_conversation_id, task_id).await
}

pub async fn lookup_scoped(
    conn: &DatabaseConnection,
    parent_conversation_id: i32,
    task_id: &str,
) -> Result<ScopedLookup, DbError> {
    let Some(row) = find_raw_by_task_id(conn, task_id).await? else {
        return Ok(ScopedLookup::Absent);
    };
    if row.parent_conversation_id != parent_conversation_id
        || !conversations_are_live(conn, parent_conversation_id, row.child_conversation_id).await?
    {
        return Ok(ScopedLookup::Hidden);
    }
    Ok(ScopedLookup::Visible(entry_from_model(row)?))
}

/// Return every durable task owned by a parent whose parent/child rows and
/// folders are still visible. A child can execute several continuation rounds,
/// so this deliberately returns task history rather than its current pointer.
pub async fn list_for_parent(
    conn: &DatabaseConnection,
    parent_conversation_id: i32,
) -> Result<Vec<TaskLedgerEntry>, DbError> {
    let rows = delegation_task::Entity::find()
        .filter(delegation_task::Column::ParentConversationId.eq(parent_conversation_id))
        .order_by_asc(delegation_task::Column::CreatedAt)
        .order_by_asc(delegation_task::Column::Id)
        .all(conn)
        .await?;
    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        if conversations_are_live(conn, parent_conversation_id, row.child_conversation_id).await? {
            entries.push(entry_from_model(row)?);
        }
    }
    Ok(entries)
}

/// Every delegation admitted while a work task executed, oldest first — the
/// task detail's "sub-agent runs" ledger.
///
/// Read straight from the storing columns rather than through
/// [`entry_from_model`]: this is a display projection, and one row with an
/// unreadable `resume_binding`/`terminal_report` (hand-edited DB, a future
/// schema the reader predates) must not blank the whole list.
pub async fn list_for_task(
    conn: &DatabaseConnection,
    work_task_id: i32,
) -> Result<Vec<crate::models::WorkTaskDelegationInfo>, DbError> {
    let rows = delegation_task::Entity::find()
        .filter(delegation_task::Column::WorkTaskId.eq(work_task_id))
        .order_by_asc(delegation_task::Column::CreatedAt)
        .order_by_asc(delegation_task::Column::Id)
        .all(conn)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| crate::models::WorkTaskDelegationInfo {
            task_id: row.task_id,
            source_task_id: row.source_task_id,
            agent_type: row.agent_type,
            // Wire-stable projection of the ledger status: `unknown` (frozen
            // by the boot sweep for a run the process abandoned) reads as
            // `interrupted` to the UI.
            status: parse_status(&row.status)
                .map(project_child_status)
                .map(str::to_owned)
                .unwrap_or(row.status),
            task: row.task,
            child_conversation_id: row.child_conversation_id,
            created_at: row.created_at,
            updated_at: row.updated_at,
            duration_ms: row.duration_ms,
            effective_model: row.effective_model,
            effective_mode: row.effective_mode,
            effective_reasoning_level: row.effective_reasoning_level,
            input_tokens: row.input_tokens,
            output_tokens: row.output_tokens,
            error_code: row.error_code,
        })
        .collect())
}

/// One `@Session`-recall row: a delegated child session of a parent
/// conversation, with the ledger-derived projection the parent's `@` panel
/// renders (design doc §14.4). Serialized straight to the frontend; the field
/// names are the wire contract mirrored in `src/lib/types.ts`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DelegatedChildSession {
    pub parent_conversation_id: i32,
    pub child_conversation_id: i32,
    pub agent_type: AgentType,
    pub title: Option<String>,
    pub git_branch: Option<String>,
    /// Wire-stable projection of the child's LATEST round:
    /// `running` | `completed` | `failed` | `canceled` | `interrupted`.
    /// `interrupted` comes exclusively from [`boot_reconcile_interrupted`]
    /// freezing a run the process abandoned — `finish` only ever writes
    /// completed/failed/canceled, so `unknown` in the ledger means "needs
    /// recovery", never "evicted from cache" (that `TaskStatus::Unknown`
    /// meaning applies to the in-memory broker path only).
    pub status: String,
    /// Whether a continuation can be admitted from the latest round right now:
    /// the round is terminal-or-interrupted AND released — exactly the
    /// precondition [`admit`] enforces on a continuation source. An
    /// `interrupted` child is continuable too (via strict recovery), which is
    /// why this flag alone must not drive the panel's "needs recovery"
    /// bucketing — pair it with `status`.
    pub continuable: bool,
    /// Rounds admitted against this child ("第 N 轮"): every continuation
    /// round reserves its own ledger row, so this is the per-child row count.
    /// For a well-formed chain (each round's `source_task_id` naming its
    /// predecessor) the count equals the chain length; counting rows keeps the
    /// derivation correct even if a link is missing.
    pub rounds: u32,
    /// The latest round's task text (the child's most recent prompt).
    pub latest_task: String,
    /// Most recent of (child conversation `updated_at`, latest round
    /// `updated_at`): the conversation row advances with transcript writes,
    /// the ledger row with finish/release, so the max is the honest
    /// "last activity" for the row.
    pub last_activity_at: DateTime<Utc>,
}

/// List a parent's delegated child sessions with their status projection, for
/// the parent composer's `@` panel (§14.4 "本次对话的子智能体"). Ledger-driven:
/// a child appears only if at least one visible ledger row names it, and
/// visibility follows [`list_for_parent`] (live parent/child/folder rows), so
/// soft-deleted children and foreign-parent tasks never surface. The global
/// history's default exclusion of delegation children is untouched — this is a
/// separate, parent-scoped entrance.
pub async fn list_child_sessions(
    conn: &DatabaseConnection,
    parent_conversation_id: i32,
) -> Result<Vec<DelegatedChildSession>, DbError> {
    let entries = list_for_parent(conn, parent_conversation_id).await?;
    let children = conversation::Entity::find()
        .filter(conversation::Column::ParentId.eq(parent_conversation_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .all(conn)
        .await?;
    Ok(project_child_sessions(&entries, &children))
}

/// Pure projection from ledger rows + child conversation rows to the
/// `@Session`-recall entries. Children with no ledger rows (pre-ledger legacy
/// spawns) are deliberately absent — the ledger is the authoritative spawn
/// record, and every broker path admits a row before sending the prompt.
/// Returns entries ordered by child id ascending (deterministic; the frontend
/// applies the §14.4 display order on top).
fn project_child_sessions(
    entries: &[TaskLedgerEntry],
    children: &[conversation::Model],
) -> Vec<DelegatedChildSession> {
    let mut by_child: BTreeMap<i32, Vec<&TaskLedgerEntry>> = BTreeMap::new();
    for entry in entries {
        by_child
            .entry(entry.child_conversation_id)
            .or_default()
            .push(entry);
    }
    let mut rows = Vec::with_capacity(by_child.len());
    for (child_id, rounds) in by_child {
        // The latest round is the highest ledger id: admission assigns
        // monotonically increasing ids, and a continuation always admits after
        // its source.
        let latest = rounds
            .iter()
            .max_by_key(|entry| entry.id)
            .expect("grouped entries are non-empty");
        let Some(child) = children
            .iter()
            .find(|child| child.id == child_id)
            .map(|child| {
                (
                    child.title.clone(),
                    child.git_branch.clone(),
                    child.updated_at,
                )
            })
        else {
            // `list_for_parent` already filtered these out; skip defensively
            // rather than emit a row that cannot be opened.
            continue;
        };
        let (title, git_branch, child_updated_at) = child;
        // Last activity = the newest write anywhere in the child's story: any
        // round's finish/release can land after a later round was admitted, so
        // this is a max over ALL rounds' `updated_at`, not just the latest's.
        let last_round_activity = rounds
            .iter()
            .map(|entry| entry.updated_at)
            .max()
            .unwrap_or(latest.updated_at);
        rows.push(DelegatedChildSession {
            parent_conversation_id: latest.parent_conversation_id,
            child_conversation_id: child_id,
            agent_type: latest.resume_binding.agent_type,
            title,
            git_branch,
            status: project_child_status(latest.status).to_owned(),
            continuable: continuation_eligible(latest),
            rounds: rounds.len() as u32,
            latest_task: latest.task.clone(),
            last_activity_at: last_round_activity.max(child_updated_at),
        });
    }
    rows
}

/// Map the latest round's ledger status onto the wire-stable projection the
/// panel buckets on. See [`DelegatedChildSession::status`].
fn project_child_status(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Canceled => "canceled",
        TaskStatus::Unknown => "interrupted",
    }
}

/// Continuation admission precondition for the latest round, mirrored from the
/// `is_terminal_status` + `released` check in [`admit`] (`unknown` counts as
/// terminal there so an interrupted run can be recovered from).
fn continuation_eligible(entry: &TaskLedgerEntry) -> bool {
    matches!(
        entry.status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Canceled | TaskStatus::Unknown
    ) && entry.released
}

/// Return the one successor reserved by `source_task_id`, if it is visible to
/// the authorized parent.
pub async fn successor(
    conn: &DatabaseConnection,
    parent_conversation_id: i32,
    source_task_id: &str,
) -> Result<Option<TaskLedgerEntry>, DbError> {
    let row = delegation_task::Entity::find()
        .filter(delegation_task::Column::ParentConversationId.eq(parent_conversation_id))
        .filter(delegation_task::Column::SourceTaskId.eq(source_task_id))
        .one(conn)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    if !conversations_are_live(conn, parent_conversation_id, row.child_conversation_id).await? {
        return Ok(None);
    }
    Ok(Some(entry_from_model(row)?))
}

/// Return the source id of an authorized task. The source row itself is not
/// required to be live here; only the task being inspected is authorized.
pub async fn source(
    conn: &DatabaseConnection,
    parent_conversation_id: i32,
    task_id: &str,
) -> Result<Option<String>, DbError> {
    Ok(lookup(conn, parent_conversation_id, task_id)
        .await?
        .and_then(|entry| entry.source_task_id))
}

/// Freeze one terminal report. The conditional UPDATE makes finish idempotent
/// and prevents a late old connection from changing a newer terminal result.
/// Returns `true` only when this call won the terminal write.
///
/// The report's queryable metrics (`error_code`, `duration_ms`, `turn_count`,
/// token counts, effective selectors) are extracted into columns by the SAME
/// statement that writes the `terminal_report` JSON — one write, no drift.
/// Metrics the report does not carry are filled from the child's usage facts on
/// that same write (see [`fill_metrics_from_token_facts`]); unknown stays NULL.
pub async fn finish(
    conn: &DatabaseConnection,
    parent_conversation_id: i32,
    task_id: &str,
    report: &DelegationTaskReport,
) -> Result<bool, DbError> {
    let Some(row) = load_for_write(conn, parent_conversation_id, task_id).await? else {
        return Err(DbError::NotFound(format!("delegation task {task_id}")));
    };
    validate_terminal_report(task_id, report, &row)?;
    let terminal_report = serde_json::to_string(report)
        .map_err(|e| DbError::Validation(format!("cannot serialize terminal report: {e}")))?;
    let mut extracted = report_metrics(report);
    fill_metrics_from_token_facts(conn, &row, &mut extracted).await;
    let result = delegation_task::Entity::update_many()
        .col_expr(
            delegation_task::Column::Status,
            sea_orm::sea_query::Expr::value(status_string(report.status)),
        )
        .col_expr(
            delegation_task::Column::TerminalReport,
            sea_orm::sea_query::Expr::value(terminal_report),
        )
        .col_expr(
            delegation_task::Column::ErrorCode,
            sea_orm::sea_query::Expr::value(extracted.error_code),
        )
        .col_expr(
            delegation_task::Column::DurationMs,
            sea_orm::sea_query::Expr::value(extracted.duration_ms),
        )
        .col_expr(
            delegation_task::Column::TurnCount,
            sea_orm::sea_query::Expr::value(extracted.turn_count),
        )
        .col_expr(
            delegation_task::Column::InputTokens,
            sea_orm::sea_query::Expr::value(extracted.input_tokens),
        )
        .col_expr(
            delegation_task::Column::OutputTokens,
            sea_orm::sea_query::Expr::value(extracted.output_tokens),
        )
        .col_expr(
            delegation_task::Column::CacheCreationTokens,
            sea_orm::sea_query::Expr::value(extracted.cache_creation_tokens),
        )
        .col_expr(
            delegation_task::Column::CacheReadTokens,
            sea_orm::sea_query::Expr::value(extracted.cache_read_tokens),
        )
        .col_expr(
            delegation_task::Column::EffectiveModel,
            sea_orm::sea_query::Expr::value(extracted.effective_model),
        )
        .col_expr(
            delegation_task::Column::EffectiveMode,
            sea_orm::sea_query::Expr::value(extracted.effective_mode),
        )
        .col_expr(
            delegation_task::Column::EffectiveReasoningLevel,
            sea_orm::sea_query::Expr::value(extracted.effective_reasoning_level),
        )
        .col_expr(
            delegation_task::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(Utc::now()),
        )
        .filter(delegation_task::Column::Id.eq(row.id))
        .filter(delegation_task::Column::Status.eq(status_string(TaskStatus::Running)))
        .filter(delegation_task::Column::TerminalReport.is_null())
        .exec(conn)
        .await?;
    Ok(result.rows_affected == 1)
}

/// The dashboard's queryable projection of a terminal report, plus the facts
/// the report cannot carry (cache split) and the model fallback. Report-carried
/// values always win; see [`fill_metrics_from_token_facts`].
struct ReportMetrics {
    error_code: Option<String>,
    duration_ms: Option<i64>,
    turn_count: Option<i32>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_creation_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    effective_model: Option<String>,
    effective_mode: Option<String>,
    effective_reasoning_level: Option<String>,
}

fn report_metrics(report: &DelegationTaskReport) -> ReportMetrics {
    // Selector wire ids: the same spellings `apply_selector_preferences`
    // inserts into `preferred_config_values`, so the column matches what the
    // child actually launched with regardless of which agent it was.
    const MODEL_OPTION: &str = crate::acp::capability_catalog::MODEL_CONFIG_OPTION_ID;
    const REASONING_OPTION: &str =
        crate::acp::capability_catalog::REASONING_EFFORT_CONFIG_OPTION_ID;

    let effective = report.selectors.as_ref().and_then(|s| s.effective.as_ref());
    ReportMetrics {
        error_code: report.error_code.clone(),
        duration_ms: report.duration_ms.map(|d| d as i64),
        turn_count: report.turn_count.map(|t| t as i32),
        // A zero here is "nothing observed", not a real count; let the token
        // facts below fill it in. The dashboard treats NULL as unknown.
        input_tokens: report
            .token_usage
            .as_ref()
            .map(|u| u.input as i64)
            .filter(|n| *n > 0),
        output_tokens: report
            .token_usage
            .as_ref()
            .map(|u| u.output as i64)
            .filter(|n| *n > 0),
        // The report itself carries no cache split yet; the ledger fills these
        // from the token facts below.
        cache_creation_tokens: None,
        cache_read_tokens: None,
        effective_model: effective
            .and_then(|e| e.config_values.get(MODEL_OPTION).cloned())
            .filter(|m| !m.trim().is_empty()),
        effective_mode: effective.and_then(|e| e.mode.clone()),
        effective_reasoning_level: effective
            .and_then(|e| e.config_values.get(REASONING_OPTION).cloned()),
    }
}

/// Fill the metrics the terminal report could not supply, from the child's
/// materialized token facts and conversation row.
///
/// The report is often thin: [`crate::acp::lifecycle`] has no per-turn usage at
/// child TurnComplete, so `token_usage` stays absent. The token sync pipeline
/// materializes the child's `token_usage_turn` rows separately; reading them
/// here is the only way the ledger learns the real spend.
///
/// Rules that keep this honest:
///   * the window starts at ADMISSION (`row.created_at`) — a reused child's
///     previous tasks' turns must not be charged to this one;
///   * a missing/empty/failed query leaves the column NULL, never 0;
///   * a value the report DID carry wins — the report is the more direct
///     observation of the child run.
///
/// When nothing is known at all, the child is synced **on demand** first (see
/// [`crate::commands::token_usage::sync_conversations`]) and the facts re-read.
/// The dashboard's materialization only runs when the user opens the Token
/// Usage page, so without this a task that just finished almost always froze
/// NULL even though its transcript held the numbers. The sync is skipped
/// entirely when any token column already has a value, keeping the common
/// terminal path free of parsing work.
///
/// Transaction boundary: `finish` itself opens no transaction (its ledger write
/// is one conditional UPDATE), and the sync commits the child's fact rows in
/// its own short transaction. Doing the sync inside a ledger transaction would
/// hold a SQLite write lock across a file parse, stalling every other writer
/// and lengthening the critical section for data the ledger does not need to
/// publish atomically with the task status. A failure at any step is logged and
/// the ledger freezes with NULLs — the terminal record matters more than its
/// token numbers.
async fn fill_metrics_from_token_facts(
    conn: &DatabaseConnection,
    row: &TaskLedgerEntry,
    metrics: &mut ReportMetrics,
) {
    if let Some(facts) = read_task_token_facts(conn, row).await {
        apply_task_facts(metrics, facts);
    }

    if token_columns_are_all_unknown(metrics) {
        // The parse path never calls back into the delegation ledger (it reads
        // conversations, approvals and the ledger rows themselves), so this
        // cannot recurse into `finish`.
        if let Err(error) =
            crate::commands::token_usage::sync_conversations(conn, &[row.child_conversation_id])
                .await
        {
            tracing::warn!(
                task_id = row.task_id,
                conversation_id = row.child_conversation_id,
                %error,
                "[delegation] on-demand token sync failed; freezing unknown tokens"
            );
        }
        // Re-read even after a failed sync attempt: a concurrent dashboard
        // pass may have materialized the same child in the meantime.
        if let Some(facts) = read_task_token_facts(conn, row).await {
            apply_task_facts(metrics, facts);
        }
    }

    // The admission snapshot can be missing (a default-model call, an agent
    // that publishes no config options, a snapshot read failure). Fall back to
    // what the conversation row or its latest usage turn names, in that order.
    if metrics.effective_model.is_none() {
        metrics.effective_model = child_model_fallback(conn, row.child_conversation_id).await;
    }
}

/// True when the ledger would render `—` in all four token columns. Only then
/// is an on-demand sync worth its parse.
fn token_columns_are_all_unknown(metrics: &ReportMetrics) -> bool {
    metrics.input_tokens.is_none()
        && metrics.output_tokens.is_none()
        && metrics.cache_creation_tokens.is_none()
        && metrics.cache_read_tokens.is_none()
}

/// One conversation's facts for THIS task's window, or `None` on "unknown".
async fn read_task_token_facts(
    conn: &DatabaseConnection,
    row: &TaskLedgerEntry,
) -> Option<token_usage_service::TaskTokenFacts> {
    match token_usage_service::task_token_facts(
        conn,
        row.child_conversation_id,
        row.created_at,
        Utc::now(),
    )
    .await
    {
        Ok(facts) => facts,
        Err(error) => {
            tracing::warn!(
                task_id = row.task_id,
                conversation_id = row.child_conversation_id,
                %error,
                "[delegation] token facts unavailable when freezing metrics"
            );
            None
        }
    }
}

/// Fold the facts in without overwriting anything the report already observed.
fn apply_task_facts(
    metrics: &mut ReportMetrics,
    facts: token_usage_service::TaskTokenFacts,
) {
    metrics.input_tokens = metrics.input_tokens.or(Some(facts.input_tokens));
    metrics.output_tokens = metrics.output_tokens.or(Some(facts.output_tokens));
    metrics.cache_creation_tokens = Some(facts.cache_creation_tokens);
    metrics.cache_read_tokens = Some(facts.cache_read_tokens);
    // A failed/canceled report has no turn count; the usage-bearing turns are
    // still a truthful lower bound.
    //
    // Scope note: a report-carried value (the normal path) comes from
    // `SessionState.turns_completed` and counts the whole session state — on a
    // REUSED child that includes earlier tasks' turns, while the token columns
    // beside it are windowed to this task. The two are therefore not the same
    // unit on a resumed child, and the report value is deliberately left
    // untouched (it is what the run itself observed). Only this fallback, used
    // when the report carried none, is already window-scoped.
    metrics.turn_count = metrics
        .turn_count
        .or_else(|| i32::try_from(facts.turn_count).ok());
}

async fn child_model_fallback(conn: &DatabaseConnection, conversation_id: i32) -> Option<String> {
    let from_row = conversation::Entity::find_by_id(conversation_id)
        .one(conn)
        .await
        .ok()
        .flatten()
        .and_then(|row| row.model)
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty());
    if from_row.is_some() {
        return from_row;
    }
    match token_usage_service::latest_model_for_conversation(conn, conversation_id).await {
        Ok(model) => model,
        Err(error) => {
            tracing::warn!(
                conversation_id,
                %error,
                "[delegation] model fallback from token facts failed"
            );
            None
        }
    }
}

/// Mark process release independently from finish. This is deliberately a
/// one-way CAS: finishing a task never resets a release acknowledgement.
pub async fn mark_released(
    conn: &DatabaseConnection,
    parent_conversation_id: i32,
    task_id: &str,
) -> Result<bool, DbError> {
    let Some(row) = load_for_write(conn, parent_conversation_id, task_id).await? else {
        return Err(DbError::NotFound(format!("delegation task {task_id}")));
    };
    let result = delegation_task::Entity::update_many()
        .col_expr(
            delegation_task::Column::Released,
            sea_orm::sea_query::Expr::value(true),
        )
        .col_expr(
            delegation_task::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(Utc::now()),
        )
        .filter(delegation_task::Column::Id.eq(row.id))
        .filter(delegation_task::Column::Released.eq(false))
        .exec(conn)
        .await?;
    Ok(result.rows_affected == 1)
}

/// Reconcile execution ownership from a previous process. No ACP child or
/// release barrier survives a restart, so every unreleased row is now released;
/// a row still marked running is frozen with an unknown interrupted outcome.
pub async fn boot_reconcile_interrupted(conn: &DatabaseConnection) -> Result<u64, DbError> {
    let txn = conn.begin().await?;
    let rows = delegation_task::Entity::find()
        .filter(delegation_task::Column::Released.eq(false))
        .all(&txn)
        .await?;
    let count = rows.len() as u64;

    for row in rows {
        let mut active = row.clone().into_active_model();
        if row.status == status_string(TaskStatus::Running) {
            let binding: ResumeBinding =
                serde_json::from_str(&row.resume_binding).map_err(|e| {
                    DbError::Migration(format!("invalid resume binding for {}: {e}", row.task_id))
                })?;
            let report = DelegationTaskReport {
                task_id: Some(row.task_id.clone()),
                status: TaskStatus::Unknown,
                child_conversation_id: Some(row.child_conversation_id),
                agent_type: Some(binding.agent_type),
                text: None,
                error_code: Some("interrupted".into()),
                message: Some(
                    "The application stopped while this delegation was running; its outcome is unknown."
                        .into(),
                ),
                duration_ms: None,
                turn_count: None,
                token_usage: None,
                blocked_on: None,
                selectors: None,
                assistant_id: None,
                assistant_name: None,
                progress: None,
            };
            active.status = Set(status_string(TaskStatus::Unknown));
            active.terminal_report = Set(Some(serde_json::to_string(&report).map_err(|e| {
                DbError::Validation(format!("cannot serialize interrupted report: {e}"))
            })?));
            // Keep the extracted column in step with the frozen JSON: an
            // interrupted row groups under its error code, with no duration /
            // token facts to invent.
            active.error_code = Set(Some("interrupted".into()));
        }
        active.released = Set(true);
        active.updated_at = Set(Utc::now());
        active.update(&txn).await?;
    }

    txn.commit().await?;
    Ok(count)
}

// ── Performance dashboard aggregation (upstream #724) ───────────────────
//
// The metrics columns above make the ledger groupable; this section is the
// read side that turns it into the sub-agent performance report. The whole
// ledger is one row per delegation ever run, so the aggregation SELECTs only
// the narrow metric columns (never the `task` / JSON blobs) and folds in
// Rust — SQL GROUP BY would buy nothing at this cardinality and would push
// the rework-chain derivation into a self-join.

/// Counters for one grouping bucket (or the all-up totals row).
///
/// Rates are pre-computed so every transport ships the same arithmetic:
///   * `success_rate` = completed / TERMINAL rows (running excluded from the
///     denominator — an in-flight task is neither success nor failure)
///   * `rework_rate` = reworked / TERMINAL rows, where a task is "reworked"
///     when another admitted task continues it (`source_task_id`). By
///     construction a source must be terminal AND released before a
///     successor may be admitted, so only terminal tasks can be reworked.
///     `resume_delegation` reuses the SAME task id (no new row), so resumed
///     interruptions do NOT inflate the rework count.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DelegationDimensionStats {
    /// The grouping key: agent slug for `by_agent`, model id for `by_model`.
    /// `None` = not recorded (pre-metrics rows / calls that asked for no
    /// selectors) — rendered as its own bucket, never silently merged.
    pub key: Option<String>,
    /// All ledger rows in the bucket, including running ones.
    pub task_count: u64,
    pub completed: u64,
    pub failed: u64,
    pub canceled: u64,
    /// Terminal-but-unknown outcome (app died mid-run; `boot_reconcile`).
    pub unknown: u64,
    /// Still running right now.
    pub running: u64,
    pub success_rate: f64,
    /// Mean wall-clock runtime over rows that REPORT a duration
    /// (`duration_ms` is NULL for pre-metrics rows and setup failures).
    pub avg_duration_ms: Option<f64>,
    /// Sum over rows that reported token usage; NULL-reporting rows
    /// contribute nothing rather than zero.
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Cache split, same NULL/zero rule as the two counters above. Without
    /// these a Claude/CodeBuddy task's total would miss most of its real cost.
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
    /// Terminal tasks in the bucket that a successor continued.
    pub reworked: u64,
    pub rework_rate: f64,
}

/// The whole dashboard payload: all-up totals plus per-agent and
/// per-effective-model breakdowns.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DelegationPerformanceReport {
    pub totals: DelegationDimensionStats,
    /// Sorted by `task_count` descending, then key.
    pub by_agent: Vec<DelegationDimensionStats>,
    /// Sorted by `task_count` descending, then key.
    pub by_model: Vec<DelegationDimensionStats>,
}

/// The narrow column set the aggregation reads. Field names mirror the
/// `delegation_task` columns so `FromQueryResult` maps them positionally by
/// name from the `select_only` query.
#[derive(Debug, sea_orm::FromQueryResult)]
struct MetricsRow {
    task_id: String,
    parent_conversation_id: i32,
    child_conversation_id: i32,
    source_task_id: Option<String>,
    status: String,
    agent_type: Option<String>,
    duration_ms: Option<i64>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_creation_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    effective_model: Option<String>,
}

/// Fold one ledger row into a stats bucket.
#[derive(Default)]
struct Bucket {
    key: Option<String>,
    task_count: u64,
    completed: u64,
    failed: u64,
    canceled: u64,
    unknown: u64,
    running: u64,
    duration_sum: f64,
    duration_count: u64,
    input_tokens: i64,
    output_tokens: i64,
    cache_creation_tokens: i64,
    cache_read_tokens: i64,
    reworked: u64,
}

impl Bucket {
    fn add(&mut self, row: &MetricsRow, terminal: bool, reworked: bool) {
        self.task_count += 1;
        match row.status.as_str() {
            "completed" => self.completed += 1,
            "failed" => self.failed += 1,
            "canceled" => self.canceled += 1,
            "running" => self.running += 1,
            _ => self.unknown += 1,
        }
        if let Some(duration) = row.duration_ms {
            self.duration_sum += duration as f64;
            self.duration_count += 1;
        }
        self.input_tokens += row.input_tokens.unwrap_or(0);
        self.output_tokens += row.output_tokens.unwrap_or(0);
        self.cache_creation_tokens += row.cache_creation_tokens.unwrap_or(0);
        self.cache_read_tokens += row.cache_read_tokens.unwrap_or(0);
        if terminal && reworked {
            self.reworked += 1;
        }
    }

    fn finish(self) -> DelegationDimensionStats {
        let terminal = self.completed + self.failed + self.canceled + self.unknown;
        DelegationDimensionStats {
            key: self.key,
            task_count: self.task_count,
            completed: self.completed,
            failed: self.failed,
            canceled: self.canceled,
            unknown: self.unknown,
            running: self.running,
            success_rate: rate(self.completed, terminal),
            avg_duration_ms: (self.duration_count > 0)
                .then(|| self.duration_sum / self.duration_count as f64),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_creation_tokens: self.cache_creation_tokens,
            cache_read_tokens: self.cache_read_tokens,
            reworked: self.reworked,
            rework_rate: rate(self.reworked, terminal),
        }
    }
}

fn rate(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

/// Aggregate the ledger into the performance report. Rows whose parent or
/// child conversation (or their folders) is soft-deleted are excluded — the
/// dashboard should show the same history the user can still open.
pub async fn performance_report(
    conn: &DatabaseConnection,
) -> Result<DelegationPerformanceReport, DbError> {
    let rows = delegation_task::Entity::find()
        .select_only()
        .column(delegation_task::Column::TaskId)
        .column(delegation_task::Column::ParentConversationId)
        .column(delegation_task::Column::ChildConversationId)
        .column(delegation_task::Column::SourceTaskId)
        .column(delegation_task::Column::Status)
        .column(delegation_task::Column::AgentType)
        .column(delegation_task::Column::DurationMs)
        .column(delegation_task::Column::InputTokens)
        .column(delegation_task::Column::OutputTokens)
        .column(delegation_task::Column::CacheCreationTokens)
        .column(delegation_task::Column::CacheReadTokens)
        .column(delegation_task::Column::EffectiveModel)
        .into_model::<MetricsRow>()
        .all(conn)
        .await?;

    let live = live_conversation_ids(conn).await?;
    // Every successor's source id — membership decides "reworked". Chains
    // (A→B→C on one child) mark A and B; C is the round the parent accepted.
    let reworked_ids: std::collections::HashSet<&str> = rows
        .iter()
        .filter_map(|row| row.source_task_id.as_deref())
        .collect();

    let mut totals = Bucket::default();
    let mut by_agent: std::collections::BTreeMap<Option<String>, Bucket> =
        std::collections::BTreeMap::new();
    let mut by_model: std::collections::BTreeMap<Option<String>, Bucket> =
        std::collections::BTreeMap::new();

    for row in &rows {
        if !live.contains(&row.parent_conversation_id) || !live.contains(&row.child_conversation_id)
        {
            continue;
        }
        let terminal = row.status != "running";
        let reworked = reworked_ids.contains(row.task_id.as_str());
        totals.add(row, terminal, reworked);
        let agent_bucket = by_agent.entry(row.agent_type.clone()).or_default();
        agent_bucket.key = row.agent_type.clone();
        agent_bucket.add(row, terminal, reworked);
        let model_bucket = by_model.entry(row.effective_model.clone()).or_default();
        model_bucket.key = row.effective_model.clone();
        model_bucket.add(row, terminal, reworked);
    }

    let sort_buckets = |mut buckets: Vec<DelegationDimensionStats>| {
        buckets.sort_by(|a, b| {
            b.task_count
                .cmp(&a.task_count)
                .then_with(|| a.key.cmp(&b.key))
        });
        buckets
    };

    Ok(DelegationPerformanceReport {
        totals: totals.finish(),
        by_agent: sort_buckets(by_agent.into_values().map(Bucket::finish).collect()),
        by_model: sort_buckets(by_model.into_values().map(Bucket::finish).collect()),
    })
}

/// Conversation ids whose row and folder are both still visible. Two narrow
/// queries instead of a per-row `conversations_are_live` probe.
async fn live_conversation_ids(
    conn: &DatabaseConnection,
) -> Result<std::collections::HashSet<i32>, DbError> {
    let conversations = conversation::Entity::find()
        .filter(conversation::Column::DeletedAt.is_null())
        .all(conn)
        .await?;
    let live_folder_ids: std::collections::HashSet<i32> = folder::Entity::find()
        .filter(folder::Column::DeletedAt.is_null())
        .all(conn)
        .await?
        .into_iter()
        .map(|f| f.id)
        .collect();
    Ok(conversations
        .into_iter()
        .filter(|c| live_folder_ids.contains(&c.folder_id))
        .map(|c| c.id)
        .collect())
}

async fn reconcile_insert_race<C: ConnectionTrait>(
    conn: &C,
    input: &AdmissionInput,
    insert_error: sea_orm::DbErr,
) -> Result<AdmissionResult, DbError> {
    let Some(source_task_id) = input.source_task_id.as_deref() else {
        return Err(insert_error.into());
    };
    let Some(winner) = delegation_task::Entity::find()
        .filter(delegation_task::Column::ParentConversationId.eq(input.parent_conversation_id))
        .filter(delegation_task::Column::SourceTaskId.eq(source_task_id))
        .one(conn)
        .await?
    else {
        return Err(insert_error.into());
    };
    let Some(entry) = load_authorized(conn, input.parent_conversation_id, &winner.task_id).await?
    else {
        return Err(DbError::Conflict(format!(
            "source task {source_task_id} is already reserved but is no longer queryable"
        )));
    };
    if same_admission_key(&winner, input)? {
        return Ok(AdmissionResult::Existing { entry });
    }
    Ok(AdmissionResult::Conflict {
        next_task_id: winner.task_id,
        reason: format!(
                    "source task {source_task_id} already has a successor with a different task, agent, or working directory"
        ),
    })
}

async fn load_authorized<C: ConnectionTrait>(
    conn: &C,
    parent_conversation_id: i32,
    task_id: &str,
) -> Result<Option<TaskLedgerEntry>, DbError> {
    let Some(row) = delegation_task::Entity::find()
        .filter(delegation_task::Column::TaskId.eq(task_id))
        .filter(delegation_task::Column::ParentConversationId.eq(parent_conversation_id))
        .one(conn)
        .await?
    else {
        return Ok(None);
    };
    if !conversations_are_live(conn, parent_conversation_id, row.child_conversation_id).await? {
        return Ok(None);
    }
    Ok(Some(entry_from_model(row)?))
}

async fn load_for_write<C: ConnectionTrait>(
    conn: &C,
    parent_conversation_id: i32,
    task_id: &str,
) -> Result<Option<TaskLedgerEntry>, DbError> {
    let row = delegation_task::Entity::find()
        .filter(delegation_task::Column::TaskId.eq(task_id))
        .filter(delegation_task::Column::ParentConversationId.eq(parent_conversation_id))
        .one(conn)
        .await?;
    row.map(entry_from_model).transpose()
}

async fn find_raw_by_task_id<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
) -> Result<Option<delegation_task::Model>, DbError> {
    Ok(delegation_task::Entity::find()
        .filter(delegation_task::Column::TaskId.eq(task_id))
        .one(conn)
        .await?)
}

async fn ensure_live_conversation<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
    label: &str,
) -> Result<(), DbError> {
    if conversations_are_live(conn, conversation_id, conversation_id).await? {
        return Ok(());
    }
    Err(DbError::NotFound(format!(
        "{label} conversation {conversation_id}"
    )))
}

async fn conversations_are_live<C: ConnectionTrait>(
    conn: &C,
    parent_conversation_id: i32,
    child_conversation_id: i32,
) -> Result<bool, DbError> {
    let Some(parent) = conversation::Entity::find_by_id(parent_conversation_id)
        .one(conn)
        .await?
    else {
        return Ok(false);
    };
    let Some(child) = conversation::Entity::find_by_id(child_conversation_id)
        .one(conn)
        .await?
    else {
        return Ok(false);
    };
    if parent.deleted_at.is_some() || child.deleted_at.is_some() {
        return Ok(false);
    }
    let Some(parent_folder) = folder::Entity::find_by_id(parent.folder_id)
        .one(conn)
        .await?
    else {
        return Ok(false);
    };
    let Some(child_folder) = folder::Entity::find_by_id(child.folder_id)
        .one(conn)
        .await?
    else {
        return Ok(false);
    };
    Ok(parent_folder.deleted_at.is_none() && child_folder.deleted_at.is_none())
}

fn entry_from_model(model: delegation_task::Model) -> Result<TaskLedgerEntry, DbError> {
    let status = parse_status(&model.status)?;
    let binding: ResumeBinding = serde_json::from_str(&model.resume_binding).map_err(|e| {
        DbError::Migration(format!("invalid resume binding for {}: {e}", model.task_id))
    })?;
    let report = match model.terminal_report.as_deref() {
        Some(raw) => serde_json::from_str(raw).map_err(|e| {
            DbError::Migration(format!(
                "invalid terminal report for {}: {e}",
                model.task_id
            ))
        })?,
        None => running_report(&model, status, binding.agent_type),
    };
    Ok(TaskLedgerEntry {
        id: model.id,
        task_id: model.task_id,
        parent_conversation_id: model.parent_conversation_id,
        child_conversation_id: model.child_conversation_id,
        source_task_id: model.source_task_id,
        task: model.task,
        requested_working_dir: model.requested_working_dir,
        resume_binding: binding,
        status,
        released: model.released,
        report,
        created_at: model.created_at,
        updated_at: model.updated_at,
        work_task_id: model.work_task_id,
    })
}

fn running_report(
    model: &delegation_task::Model,
    status: TaskStatus,
    agent_type: AgentType,
) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: Some(model.task_id.clone()),
        status,
        child_conversation_id: Some(model.child_conversation_id),
        agent_type: Some(agent_type),
        text: None,
        error_code: None,
        message: None,
        duration_ms: None,
        turn_count: None,
        token_usage: None,
        blocked_on: None,
        selectors: None,
        assistant_id: None,
        assistant_name: None,
        progress: None,
    }
}

fn validate_input(input: &AdmissionInput) -> Result<(), DbError> {
    if input.task_id.trim().is_empty() {
        return Err(DbError::Validation("task id must not be empty".into()));
    }
    if input.task.trim().is_empty() {
        return Err(DbError::Validation("task must not be empty".into()));
    }
    if input.resume_binding.child_conversation_id != input.child_conversation_id {
        return Err(DbError::Validation(
            "resume binding child conversation does not match admission".into(),
        ));
    }
    if input.resume_binding.external_session_id.trim().is_empty()
        || input.resume_binding.working_dir.trim().is_empty()
        || input.resume_binding.config_fingerprint.trim().is_empty()
    {
        return Err(DbError::Validation(
            "resume binding requires session id, working directory, and config fingerprint".into(),
        ));
    }
    Ok(())
}

fn validate_terminal_report(
    task_id: &str,
    report: &DelegationTaskReport,
    entry: &TaskLedgerEntry,
) -> Result<(), DbError> {
    if !is_terminal(report.status) {
        return Err(DbError::Validation(
            "only completed, failed, or canceled reports can finish a task".into(),
        ));
    }
    if report.task_id.as_deref() != Some(task_id) {
        return Err(DbError::Validation(
            "terminal report must carry the admitted task id".into(),
        ));
    }
    if report.child_conversation_id != Some(entry.child_conversation_id) {
        return Err(DbError::Validation(
            "terminal report child conversation does not match admission".into(),
        ));
    }
    if report.agent_type != Some(entry.resume_binding.agent_type) {
        return Err(DbError::Validation(
            "terminal report agent does not match admission".into(),
        ));
    }
    Ok(())
}

fn same_admission_key(
    row: &delegation_task::Model,
    input: &AdmissionInput,
) -> Result<bool, DbError> {
    let binding: ResumeBinding = serde_json::from_str(&row.resume_binding).map_err(|e| {
        DbError::Migration(format!("invalid resume binding for {}: {e}", row.task_id))
    })?;
    Ok(row.task == input.task && binding == input.resume_binding)
}

fn is_terminal(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Canceled
    )
}

fn is_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "canceled" | "unknown")
}

fn status_string(status: TaskStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn parse_status(status: &str) -> Result<TaskStatus, DbError> {
    serde_json::from_value(serde_json::Value::String(status.to_owned()))
        .map_err(|e| DbError::Migration(format!("invalid delegation task status {status:?}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::service::{conversation_service, folder_service};
    use crate::db::test_helpers::{fresh_disk_db, fresh_in_memory_db};
    use sea_orm::Database;

    fn binding(child: i32) -> ResumeBinding {
        ResumeBinding {
            agent_type: AgentType::Codex,
            external_session_id: format!("session-{child}"),
            child_conversation_id: child,
            working_dir: "/workspace/project".into(),
            preferred_mode_id: Some("default".into()),
            preferred_config_values: BTreeMap::from([(String::from("model"), String::from("o3"))]),
            config_fingerprint: "fingerprint-1".into(),
        }
    }

    fn input(
        task_id: &str,
        parent: i32,
        child: i32,
        source: Option<&str>,
        task: &str,
    ) -> AdmissionInput {
        AdmissionInput {
            task_id: task_id.into(),
            parent_conversation_id: parent,
            child_conversation_id: child,
            source_task_id: source.map(str::to_owned),
            task: task.into(),
            requested_working_dir: Some("/workspace/project".into()),
            resume_binding: binding(child),
            work_task_id: None,
        }
    }

    async fn conversations(db: &crate::db::AppDatabase) -> (i32, i32) {
        let folder = folder_service::add_folder(&db.conn, "/workspace/project")
            .await
            .expect("folder")
            .id;
        let parent =
            conversation_service::create(&db.conn, folder, AgentType::ClaudeCode, None, None)
                .await
                .expect("parent")
                .id;
        let child = conversation_service::create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("child")
            .id;
        (parent, child)
    }

    fn report(task_id: &str, child: i32, text: &str, status: TaskStatus) -> DelegationTaskReport {
        DelegationTaskReport {
            task_id: Some(task_id.into()),
            status,
            child_conversation_id: Some(child),
            agent_type: Some(AgentType::Codex),
            text: Some(text.into()),
            error_code: None,
            message: None,
            duration_ms: Some(12),
            turn_count: None,
            token_usage: None,
            blocked_on: None,
            selectors: None,
            assistant_id: None,
            assistant_name: None,
            progress: None,
        }
    }

    #[tokio::test]
    async fn terminal_reports_survive_disk_reopen_and_child_status_drift() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = fresh_disk_db(dir.path()).await;
        let (parent, child) = conversations(&db).await;
        let result = admit(&db.conn, input("t0", parent, child, None, "first"))
            .await
            .expect("admit");
        assert!(matches!(result, AdmissionResult::New { .. }));

        let first = report("t0", child, "original", TaskStatus::Completed);
        assert!(finish(&db.conn, parent, "t0", &first)
            .await
            .expect("finish"));
        mark_released(&db.conn, parent, "t0")
            .await
            .expect("release t0");
        admit(
            &db.conn,
            input("t1", parent, child, Some("t0"), "follow-up"),
        )
        .await
        .expect("admit t1");
        assert!(finish(
            &db.conn,
            parent,
            "t1",
            &report("t1", child, "follow-up", TaskStatus::Completed),
        )
        .await
        .expect("finish t1"));
        assert!(!finish(
            &db.conn,
            parent,
            "t0",
            &report("t0", child, "late overwrite", TaskStatus::Failed),
        )
        .await
        .expect("idempotent finish"));
        conversation_service::update_status(
            &db.conn,
            child,
            conversation::ConversationStatus::Cancelled,
        )
        .await
        .expect("child status");
        db.conn.close().await.expect("close disk db");

        let path = dir.path().join("source.db");
        let reopened = Database::connect(format!("sqlite:{}?mode=rwc", path.to_string_lossy()))
            .await
            .expect("reopen disk db");

        let entry = lookup(&reopened, parent, "t0")
            .await
            .expect("lookup")
            .expect("entry");
        assert_eq!(
            serde_json::to_value(&entry.report).expect("stored report"),
            serde_json::to_value(&first).expect("expected report"),
        );
        assert_eq!(entry.status, TaskStatus::Completed);
        let successor_entry = lookup(&reopened, parent, "t1")
            .await
            .expect("lookup t1")
            .expect("t1 entry");
        assert_eq!(successor_entry.report.text.as_deref(), Some("follow-up"));
        reopened.close().await.expect("close reopened db");
    }

    #[tokio::test]
    async fn boot_reconcile_repairs_unreleased_rows_after_disk_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = fresh_disk_db(dir.path()).await;
        let (parent, child) = conversations(&db).await;
        admit(
            &db.conn,
            input("running", parent, child, None, "running task"),
        )
        .await
        .expect("admit running");
        admit(&db.conn, input("done", parent, child, None, "done task"))
            .await
            .expect("admit done");
        let done_report = report("done", child, "finished", TaskStatus::Completed);
        assert!(finish(&db.conn, parent, "done", &done_report)
            .await
            .expect("finish done"));
        db.conn.close().await.expect("close disk db");

        let path = dir.path().join("source.db");
        let reopened = Database::connect(format!("sqlite:{}?mode=rwc", path.to_string_lossy()))
            .await
            .expect("reopen disk db");
        assert_eq!(boot_reconcile_interrupted(&reopened).await.unwrap(), 2);
        assert_eq!(boot_reconcile_interrupted(&reopened).await.unwrap(), 0);

        let interrupted = lookup(&reopened, parent, "running").await.unwrap().unwrap();
        assert_eq!(interrupted.status, TaskStatus::Unknown);
        assert!(interrupted.released);
        assert_eq!(
            interrupted.report.error_code.as_deref(),
            Some("interrupted")
        );

        let done = lookup(&reopened, parent, "done").await.unwrap().unwrap();
        assert_eq!(done.status, TaskStatus::Completed);
        assert!(done.released);
        assert_eq!(done.report.text.as_deref(), Some("finished"));

        assert!(matches!(
            admit_continuation(
                &reopened,
                input("continued", parent, child, Some("running"), "continue"),
            )
            .await
            .unwrap(),
            AdmissionResult::New { .. }
        ));
        reopened.close().await.expect("close reopened db");
    }

    #[tokio::test]
    async fn lookup_requires_parent_and_live_rows() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("t0", parent, child, None, "first"))
            .await
            .expect("admit");
        let other_parent = {
            let folder = folder_service::add_folder(&db.conn, "/workspace/other")
                .await
                .expect("folder")
                .id;
            conversation_service::create(&db.conn, folder, AgentType::ClaudeCode, None, None)
                .await
                .expect("other parent")
                .id
        };
        assert!(lookup(&db.conn, other_parent, "t0")
            .await
            .expect("auth lookup")
            .is_none());
        conversation_service::soft_delete(&db.conn, child)
            .await
            .expect("delete child");
        assert!(lookup(&db.conn, parent, "t0")
            .await
            .expect("deleted lookup")
            .is_none());

        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("folder-task", parent, child, None, "task"))
            .await
            .expect("admit folder task");
        let folder_id = conversation::Entity::find_by_id(parent)
            .one(&db.conn)
            .await
            .expect("parent row")
            .expect("parent")
            .folder_id;
        folder_service::soft_delete_folder(&db.conn, folder_id)
            .await
            .expect("delete folder");
        assert!(lookup(&db.conn, parent, "folder-task")
            .await
            .expect("folder-deleted lookup")
            .is_none());

        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("parent-task", parent, child, None, "task"))
            .await
            .expect("admit parent task");
        conversation_service::soft_delete(&db.conn, parent)
            .await
            .expect("delete parent");
        assert!(lookup(&db.conn, parent, "parent-task")
            .await
            .expect("parent-deleted lookup")
            .is_none());
    }

    #[tokio::test]
    async fn deleted_parent_still_allows_finish_release_before_restore_lookup() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("t0", parent, child, None, "first"))
            .await
            .expect("admit");
        let folder_id = conversation::Entity::find_by_id(parent)
            .one(&db.conn)
            .await
            .expect("parent row")
            .expect("parent")
            .folder_id;

        conversation_service::soft_delete(&db.conn, parent)
            .await
            .expect("delete parent");
        let done = report("t0", child, "done", TaskStatus::Completed);
        assert!(finish(&db.conn, parent, "t0", &done)
            .await
            .expect("finish after parent delete"));
        assert!(mark_released(&db.conn, parent, "t0")
            .await
            .expect("release after parent delete"));
        assert!(
            conversation_service::restore_soft_deleted(&db.conn, parent, folder_id)
                .await
                .expect("restore parent")
        );

        let entry = lookup(&db.conn, parent, "t0")
            .await
            .expect("cold lookup")
            .expect("restored entry");
        assert_eq!(entry.status, TaskStatus::Completed);
        assert!(entry.released);
        assert_eq!(
            serde_json::to_value(&entry.report).expect("stored report"),
            serde_json::to_value(&done).expect("expected report"),
        );
    }

    #[tokio::test]
    async fn incomplete_or_unreleased_source_rejection_does_not_reserve_slot() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("t0", parent, child, None, "first"))
            .await
            .expect("admit t0");
        finish(
            &db.conn,
            parent,
            "t0",
            &report("t0", child, "done", TaskStatus::Completed),
        )
        .await
        .expect("finish t0");
        assert!(
            admit(&db.conn, input("t1", parent, child, Some("t0"), "next"))
                .await
                .is_err()
        );
        mark_released(&db.conn, parent, "t0")
            .await
            .expect("release t0");
        assert!(matches!(
            admit(&db.conn, input("t1", parent, child, Some("t0"), "next"))
                .await
                .expect("admit after release"),
            AdmissionResult::New { .. }
        ));

        mark_released(&db.conn, parent, "t1")
            .await
            .expect("release running t1");
        assert!(
            admit(&db.conn, input("t2", parent, child, Some("t1"), "next"))
                .await
                .is_err()
        );
        finish(
            &db.conn,
            parent,
            "t1",
            &report("t1", child, "done", TaskStatus::Completed),
        )
        .await
        .expect("finish t1");
        assert!(matches!(
            admit(&db.conn, input("t2", parent, child, Some("t1"), "next"))
                .await
                .expect("admit after finish"),
            AdmissionResult::New { .. }
        ));
    }

    #[tokio::test]
    async fn continuation_admission_and_child_pointer_commit_or_rollback_together() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("t0", parent, child, None, "first"))
            .await
            .unwrap();
        finish(
            &db.conn,
            parent,
            "t0",
            &report("t0", child, "done", TaskStatus::Completed),
        )
        .await
        .unwrap();
        mark_released(&db.conn, parent, "t0").await.unwrap();
        conversation_service::advance_delegation_call_id(&db.conn, child, "t0", "taken")
            .await
            .unwrap();

        assert!(
            admit_continuation(&db.conn, input("t1", parent, child, Some("t0"), "next"))
                .await
                .is_err()
        );
        assert!(successor(&db.conn, parent, "t0").await.unwrap().is_none());
        let row = conversation::Entity::find_by_id(child)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.delegation_call_id.as_deref(), Some("taken"));

        conversation_service::advance_delegation_call_id(&db.conn, child, "taken", "t0")
            .await
            .unwrap();
        assert!(matches!(
            admit_continuation(&db.conn, input("t1", parent, child, Some("t0"), "next"))
                .await
                .unwrap(),
            AdmissionResult::New { .. }
        ));
        let row = conversation::Entity::find_by_id(child)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.delegation_call_id.as_deref(), Some("t1"));
    }

    #[tokio::test]
    async fn source_slot_race_returns_one_existing_successor() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("t0", parent, child, None, "first"))
            .await
            .expect("admit source");
        let done = report("t0", child, "done", TaskStatus::Completed);
        finish(&db.conn, parent, "t0", &done).await.expect("finish");
        mark_released(&db.conn, parent, "t0")
            .await
            .expect("release");
        let left = input("t1-left", parent, child, Some("t0"), "next");
        let mut right = left.clone();
        right.task_id = "t1-right".into();
        let (left, right) = tokio::join!(admit(&db.conn, left), admit(&db.conn, right));
        let mut new_count = 0;
        let mut existing_count = 0;
        let ids = [left, right]
            .into_iter()
            .map(|result| match result.expect("admission") {
                AdmissionResult::New { entry } => {
                    new_count += 1;
                    entry.task_id
                }
                AdmissionResult::Existing { entry } => {
                    existing_count += 1;
                    entry.task_id
                }
                AdmissionResult::Conflict { .. } => panic!("same key must be idempotent"),
            })
            .collect::<Vec<_>>();
        assert_eq!(new_count, 1);
        assert_eq!(existing_count, 1);
        assert_eq!(ids[0], ids[1]);
        assert!(successor(&db.conn, parent, "t0")
            .await
            .expect("successor")
            .is_some());
    }

    #[tokio::test]
    async fn different_successor_task_is_a_conflict_with_next_id() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("t0", parent, child, None, "first"))
            .await
            .expect("admit source");
        finish(
            &db.conn,
            parent,
            "t0",
            &report("t0", child, "done", TaskStatus::Completed),
        )
        .await
        .expect("finish");
        mark_released(&db.conn, parent, "t0")
            .await
            .expect("release");
        admit(&db.conn, input("t1", parent, child, Some("t0"), "next"))
            .await
            .expect("first successor");
        let different = input("t2", parent, child, Some("t0"), "different");
        let result = admit(&db.conn, different).await.expect("conflict");
        assert!(matches!(
            result,
            AdmissionResult::Conflict { next_task_id, .. } if next_task_id == "t1"
        ));
    }

    #[tokio::test]
    async fn physical_child_delete_cascades_ledger_row() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("t0", parent, child, None, "first"))
            .await
            .expect("admit");
        assert!(delegation_task::Entity::find()
            .filter(delegation_task::Column::TaskId.eq("t0"))
            .one(&db.conn)
            .await
            .expect("ledger lookup")
            .is_some());

        conversation::Entity::delete_by_id(child)
            .exec(&db.conn)
            .await
            .expect("physical child delete");
        assert!(delegation_task::Entity::find()
            .filter(delegation_task::Column::TaskId.eq("t0"))
            .one(&db.conn)
            .await
            .expect("ledger lookup after cascade")
            .is_none());
    }

    #[tokio::test]
    async fn continuation_must_keep_the_source_session_binding() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("t0", parent, child, None, "first"))
            .await
            .expect("admit source");
        finish(
            &db.conn,
            parent,
            "t0",
            &report("t0", child, "done", TaskStatus::Completed),
        )
        .await
        .expect("finish");
        mark_released(&db.conn, parent, "t0")
            .await
            .expect("release");
        let mut different_session = input("t1", parent, child, Some("t0"), "next");
        different_session.resume_binding.external_session_id = "other".into();
        let error = admit(&db.conn, different_session)
            .await
            .expect_err("different session must be refused");
        assert!(error.to_string().contains("binding"));
        assert!(successor(&db.conn, parent, "t0")
            .await
            .expect("successor lookup")
            .is_none());
    }

    #[tokio::test]
    async fn release_and_finish_are_independent_in_both_orders() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        for (task_id, finish_first) in [("t0", true), ("t1", false)] {
            admit(&db.conn, input(task_id, parent, child, None, "task"))
                .await
                .expect("admit");
            if finish_first {
                finish(
                    &db.conn,
                    parent,
                    task_id,
                    &report(task_id, child, "done", TaskStatus::Completed),
                )
                .await
                .expect("finish");
                mark_released(&db.conn, parent, task_id)
                    .await
                    .expect("release");
            } else {
                mark_released(&db.conn, parent, task_id)
                    .await
                    .expect("release");
                finish(
                    &db.conn,
                    parent,
                    task_id,
                    &report(task_id, child, "done", TaskStatus::Completed),
                )
                .await
                .expect("finish");
            }
            let entry = lookup(&db.conn, parent, task_id)
                .await
                .expect("lookup")
                .expect("entry");
            assert!(entry.released);
            assert_eq!(entry.status, TaskStatus::Completed);
        }
    }

    // -------- @Session recall projection ---------------------------------------

    use crate::acp::delegation::spawner::DelegationLink;

    /// A delegation child row linked to its parent the way the spawner does
    /// (`parent_id` set, `kind == delegate`), so the projection's
    /// conversation-side join matches production shape.
    async fn linked_child(db: &crate::db::AppDatabase, parent: i32, title: Option<&str>) -> i32 {
        let folder = conversation::Entity::find_by_id(parent)
            .one(&db.conn)
            .await
            .expect("parent row")
            .expect("parent")
            .folder_id;
        conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            title.map(str::to_owned),
            Some("feature/child".to_owned()),
            Some(DelegationLink {
                assistant: None,
                parent_conversation_id: parent,
                parent_tool_use_id: format!("toolu_{parent}"),
                delegation_call_id: format!("call_{parent}"),
                admission: None,
            }),
        )
        .await
        .expect("linked child")
        .id
    }

    /// admit → finish → release one round against `child`.
    async fn settle(
        db: &crate::db::AppDatabase,
        parent: i32,
        child: i32,
        task_id: &str,
        source: Option<&str>,
        task: &str,
        status: TaskStatus,
    ) {
        admit(&db.conn, input(task_id, parent, child, source, task))
            .await
            .expect("admit");
        finish(
            &db.conn,
            parent,
            task_id,
            &report(task_id, child, "done", status),
        )
        .await
        .expect("finish");
        mark_released(&db.conn, parent, task_id)
            .await
            .expect("release");
    }

    #[tokio::test]
    async fn child_sessions_empty_without_ledger_rows_or_link() {
        let db = fresh_in_memory_db().await;
        let (parent, _plain_child) = conversations(&db).await;
        // A child conversation exists and is linked, but no ledger row was ever
        // admitted — the projection must not invent an entry.
        let linked = linked_child(&db, parent, Some("never ran")).await;
        assert_ne!(linked, _plain_child);

        assert!(list_child_sessions(&db.conn, parent)
            .await
            .expect("no ledger rows")
            .is_empty());

        // A ledger row alone (child row not linked to this parent) is equally
        // invisible: the conversation-side join drops it.
        settle(
            &db,
            parent,
            _plain_child,
            "t0",
            None,
            "task",
            TaskStatus::Completed,
        )
        .await;
        assert!(list_child_sessions(&db.conn, parent)
            .await
            .expect("unlinked child")
            .is_empty());
    }

    #[tokio::test]
    async fn child_sessions_project_rounds_status_and_continuity() {
        let db = fresh_in_memory_db().await;
        let (parent, _unused) = conversations(&db).await;

        // Multi-round chain: two settled rounds + a third still running. The
        // chain derives round 3 from the row count, and the projection must
        // follow the LATEST round (running), not the settled ones.
        let chained = linked_child(&db, parent, Some("Chained child")).await;
        settle(
            &db,
            parent,
            chained,
            "c0",
            None,
            "first",
            TaskStatus::Completed,
        )
        .await;
        settle(
            &db,
            parent,
            chained,
            "c1",
            Some("c0"),
            "second",
            TaskStatus::Failed,
        )
        .await;
        admit(&db.conn, input("c2", parent, chained, Some("c1"), "third"))
            .await
            .expect("admit running round");

        // Single settled round: completed + released → continuable.
        let done = linked_child(&db, parent, Some("Done child")).await;
        settle(
            &db,
            parent,
            done,
            "d0",
            None,
            "one shot",
            TaskStatus::Completed,
        )
        .await;

        // Finished but NOT released: terminal, yet not continuable (the
        // continuation admission precondition fails on `released`).
        let unreleased = linked_child(&db, parent, None).await;
        admit(
            &db.conn,
            input("u0", parent, unreleased, None, "pending release"),
        )
        .await
        .expect("admit");
        finish(
            &db.conn,
            parent,
            "u0",
            &report("u0", unreleased, "done", TaskStatus::Canceled),
        )
        .await
        .expect("finish without release");

        let rows = list_child_sessions(&db.conn, parent)
            .await
            .expect("projection");
        assert_eq!(rows.len(), 3);

        let by_id = |id: i32| rows.iter().find(|row| row.child_conversation_id == id);

        let chain = by_id(chained).expect("chained entry");
        assert_eq!(chain.rounds, 3);
        assert_eq!(chain.status, "running");
        assert!(!chain.continuable);
        assert_eq!(chain.latest_task, "third");
        assert_eq!(chain.title.as_deref(), Some("Chained child"));
        assert_eq!(chain.git_branch.as_deref(), Some("feature/child"));
        assert_eq!(chain.agent_type, AgentType::Codex);
        assert_eq!(chain.parent_conversation_id, parent);

        let done_row = by_id(done).expect("done entry");
        assert_eq!(done_row.rounds, 1);
        assert_eq!(done_row.status, "completed");
        assert!(done_row.continuable);

        let unreleased_row = by_id(unreleased).expect("unreleased entry");
        assert_eq!(unreleased_row.status, "canceled");
        assert!(!unreleased_row.continuable);

        // Last activity reflects the newest write in the child's story: the
        // still-running round's admission (or anything later) — never a stale
        // earlier round.
        let running_round = lookup(&db.conn, parent, "c2")
            .await
            .expect("lookup c2")
            .expect("c2 entry");
        assert!(chain.last_activity_at >= running_round.created_at);
    }

    #[tokio::test]
    async fn interrupted_child_projects_needs_recovery_and_stays_continuable() {
        let db = fresh_in_memory_db().await;
        let (parent, _unused) = conversations(&db).await;
        let child = linked_child(&db, parent, Some("Crashed run")).await;
        admit(&db.conn, input("r0", parent, child, None, "running task"))
            .await
            .expect("admit");
        // Simulate the process dying mid-run: boot reconcile freezes the
        // unreleased running row as `unknown` + released.
        assert_eq!(
            boot_reconcile_interrupted(&db.conn)
                .await
                .expect("reconcile"),
            1
        );

        let rows = list_child_sessions(&db.conn, parent)
            .await
            .expect("projection");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "interrupted");
        // Recovery continuation is admissible from an interrupted source, so
        // the row stays continuable — the panel buckets it as "needs recovery"
        // via `status`, not via `continuable`.
        assert!(rows[0].continuable);
        assert_eq!(rows[0].rounds, 1);

        // A strict-recovery continuation admitted afterwards flips the
        // projection back to running with two rounds.
        admit(&db.conn, input("r1", parent, child, Some("r0"), "recover"))
            .await
            .expect("recover admit");
        let rows = list_child_sessions(&db.conn, parent)
            .await
            .expect("projection after recovery");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rounds, 2);
        assert_eq!(rows[0].status, "running");
        assert!(!rows[0].continuable);
        assert_eq!(rows[0].latest_task, "recover");
    }

    #[tokio::test]
    async fn child_sessions_respect_visibility_of_child_and_folder() {
        let db = fresh_in_memory_db().await;
        let (parent, _unused) = conversations(&db).await;
        let child = linked_child(&db, parent, None).await;
        settle(
            &db,
            parent,
            child,
            "v0",
            None,
            "task",
            TaskStatus::Completed,
        )
        .await;
        assert_eq!(
            list_child_sessions(&db.conn, parent)
                .await
                .expect("visible")
                .len(),
            1
        );

        // Soft-deleted child: hidden from the ledger's authorized lookup.
        conversation_service::soft_delete(&db.conn, child)
            .await
            .expect("delete child");
        assert!(list_child_sessions(&db.conn, parent)
            .await
            .expect("child deleted")
            .is_empty());

        // Soft-deleted folder hides every row under it (parent included).
        let (parent2, _unused2) = conversations(&db).await;
        let child2 = linked_child(&db, parent2, None).await;
        settle(
            &db,
            parent2,
            child2,
            "w0",
            None,
            "task",
            TaskStatus::Completed,
        )
        .await;
        let folder_id = conversation::Entity::find_by_id(parent2)
            .one(&db.conn)
            .await
            .expect("parent2 row")
            .expect("parent2")
            .folder_id;
        folder_service::soft_delete_folder(&db.conn, folder_id)
            .await
            .expect("delete folder");
        assert!(list_child_sessions(&db.conn, parent2)
            .await
            .expect("folder deleted")
            .is_empty());
    }

    #[tokio::test]
    async fn child_sessions_are_scoped_to_their_own_parent() {
        let db = fresh_in_memory_db().await;
        let (parent_a, _a) = conversations(&db).await;
        let folder = folder_service::add_folder(&db.conn, "/workspace/other")
            .await
            .expect("folder")
            .id;
        let parent_b =
            conversation_service::create(&db.conn, folder, AgentType::ClaudeCode, None, None)
                .await
                .expect("parent b")
                .id;

        let child_a = linked_child(&db, parent_a, None).await;
        let child_b = linked_child(&db, parent_b, None).await;
        settle(
            &db,
            parent_a,
            child_a,
            "a0",
            None,
            "a task",
            TaskStatus::Completed,
        )
        .await;
        settle(
            &db,
            parent_b,
            child_b,
            "b0",
            None,
            "b task",
            TaskStatus::Completed,
        )
        .await;

        let rows_a = list_child_sessions(&db.conn, parent_a)
            .await
            .expect("parent a");
        assert_eq!(rows_a.len(), 1);
        assert_eq!(rows_a[0].child_conversation_id, child_a);
        assert_eq!(rows_a[0].latest_task, "a task");

        let rows_b = list_child_sessions(&db.conn, parent_b)
            .await
            .expect("parent b");
        assert_eq!(rows_b.len(), 1);
        assert_eq!(rows_b[0].child_conversation_id, child_b);
    }

    // ── Performance dashboard (#724) ──────────────────────────────────────

    use crate::acp::delegation::types::{
        AppliedSelectors, DelegationSelectorReport, SelectorPreferences, TokenUsage,
    };

    async fn metrics_row(conn: &DatabaseConnection, task_id: &str) -> delegation_task::Model {
        delegation_task::Entity::find()
            .filter(delegation_task::Column::TaskId.eq(task_id))
            .one(conn)
            .await
            .expect("metrics lookup")
            .expect("row exists")
    }

    /// Materialize one turn in `token_usage_turn` exactly as the token sync
    /// pipeline would (same service call, same stamp), so the freeze path sees
    /// production-shaped facts.
    async fn seed_usage(
        conn: &DatabaseConnection,
        conversation_id: i32,
        facts: Vec<crate::db::service::token_usage_service::UsageFact>,
    ) {
        crate::db::service::token_usage_service::replace_conversation_facts(
            conn,
            conversation_id,
            Utc::now(),
            &facts,
        )
        .await
        .expect("seed token facts");
    }

    fn usage_fact(
        turn_key: &str,
        occurred_at: DateTime<Utc>,
        model: Option<&str>,
        input: i64,
        output: i64,
        cache_creation: i64,
        cache_read: i64,
    ) -> crate::db::service::token_usage_service::UsageFact {
        crate::db::service::token_usage_service::UsageFact {
            turn_key: turn_key.into(),
            occurred_at,
            model: model.map(str::to_owned),
            input_tokens: input,
            output_tokens: output,
            cache_creation_tokens: cache_creation,
            cache_read_tokens: cache_read,
            duration_ms: 0,
        }
    }

    /// `finish` must extract every queryable metric from the report in the
    /// same statement that freezes the JSON — the columns and the blob can
    /// never disagree.
    #[tokio::test]
    async fn finish_extracts_report_metrics_into_columns() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("t0", parent, child, None, "work"))
            .await
            .expect("admit");

        // agent_type lands at admission, before anything terminal exists.
        let admitted = metrics_row(&db.conn, "t0").await;
        assert_eq!(admitted.agent_type.as_deref(), Some("codex"));
        assert!(admitted.duration_ms.is_none());

        // The report has no cache split; the child's materialized facts do.
        // Report-carried input/output keep precedence over the facts.
        seed_usage(
            &db.conn,
            child,
            vec![usage_fact(
                "turn-1",
                Utc::now(),
                Some("gpt-6-astra"),
                999,
                111,
                222,
                333,
            )],
        )
        .await;

        let mut done = report("t0", child, "done", TaskStatus::Completed);
        done.duration_ms = Some(1_500);
        done.turn_count = Some(4);
        done.token_usage = Some(TokenUsage {
            input: 1_200,
            output: 340,
        });
        done.selectors = Some(DelegationSelectorReport {
            requested: SelectorPreferences {
                model: Some("gpt-6-astra".into()),
                ..SelectorPreferences::default()
            },
            effective: Some(AppliedSelectors {
                mode: Some("full-auto".into()),
                config_values: BTreeMap::from([
                    (String::from("model"), String::from("gpt-6-astra")),
                    (String::from("reasoning_effort"), String::from("high")),
                ]),
            }),
        });
        assert!(finish(&db.conn, parent, "t0", &done).await.expect("finish"));

        let row = metrics_row(&db.conn, "t0").await;
        assert_eq!(row.agent_type.as_deref(), Some("codex"));
        assert_eq!(row.error_code, None);
        assert_eq!(row.duration_ms, Some(1_500));
        assert_eq!(row.turn_count, Some(4));
        assert_eq!(row.input_tokens, Some(1_200));
        assert_eq!(row.output_tokens, Some(340));
        assert_eq!(row.cache_creation_tokens, Some(222));
        assert_eq!(row.cache_read_tokens, Some(333));
        assert_eq!(row.effective_model.as_deref(), Some("gpt-6-astra"));
        assert_eq!(row.effective_mode.as_deref(), Some("full-auto"));
        assert_eq!(row.effective_reasoning_level.as_deref(), Some("high"));
    }

    /// The token pipeline is where a delegation's real spend lives; the
    /// terminal report carries none. The freeze path must sum the child's
    /// facts INSIDE the task window only — a reused child's earlier turns are
    /// a different task's cost.
    #[tokio::test]
    async fn finish_fills_tokens_and_cache_from_child_facts_inside_the_task_window() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("t0", parent, child, None, "work"))
            .await
            .expect("admit");

        let now = Utc::now();
        seed_usage(
            &db.conn,
            child,
            vec![
                // Pre-admission history of a reused child: must not count.
                usage_fact("old", now - chrono::Duration::hours(1), Some("old"), 5_000, 500, 0, 0),
                usage_fact("turn-1", now, Some("gpt-6"), 700, 100, 200, 300),
                usage_fact("turn-2", now, Some("gpt-6"), 300, 100, 100, 100),
            ],
        )
        .await;

        let done = report("t0", child, "done", TaskStatus::Completed);
        assert!(finish(&db.conn, parent, "t0", &done).await.expect("finish"));

        let row = metrics_row(&db.conn, "t0").await;
        assert_eq!(row.input_tokens, Some(1_000));
        assert_eq!(row.output_tokens, Some(200));
        assert_eq!(row.cache_creation_tokens, Some(300));
        assert_eq!(row.cache_read_tokens, Some(400));
        assert_eq!(row.turn_count, Some(2), "usage-bearing turns in the window");
    }

    /// No facts (or a zero-only row) is UNKNOWN, not zero: the dashboard must
    /// keep rendering "—" rather than a confident 0.
    #[tokio::test]
    async fn finish_keeps_tokens_null_when_child_facts_are_empty() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("t0", parent, child, None, "work"))
            .await
            .expect("admit");
        seed_usage(
            &db.conn,
            child,
            vec![usage_fact("zero", Utc::now(), None, 0, 0, 0, 0)],
        )
        .await;

        let done = report("t0", child, "done", TaskStatus::Completed);
        assert!(finish(&db.conn, parent, "t0", &done).await.expect("finish"));

        let row = metrics_row(&db.conn, "t0").await;
        assert_eq!(row.input_tokens, None);
        assert_eq!(row.output_tokens, None);
        assert_eq!(row.cache_creation_tokens, None);
        assert_eq!(row.cache_read_tokens, None);
    }

    /// Default-model calls carry no effective selectors; the conversation row
    /// is the first fallback, the latest usage turn the second.
    #[tokio::test]
    async fn finish_falls_back_to_conversation_then_usage_model() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("row-model", parent, child, None, "work"))
            .await
            .expect("admit");
        conversation_service::seed_model_if_empty(&db.conn, child, "claude-opus-4-6")
            .await
            .expect("seed conversation model");
        seed_usage(
            &db.conn,
            child,
            vec![usage_fact("turn-1", Utc::now(), Some("claude-sonnet-4-5"), 10, 1, 0, 0)],
        )
        .await;

        let done = report("row-model", child, "done", TaskStatus::Completed);
        assert!(finish(&db.conn, parent, "row-model", &done)
            .await
            .expect("finish"));
        assert_eq!(
            metrics_row(&db.conn, "row-model").await.effective_model.as_deref(),
            Some("claude-opus-4-6"),
            "the conversation row wins over per-turn models"
        );

        // A second task on a child whose row has no model falls through to the
        // latest usage turn.
        let (parent2, child2) = conversations(&db).await;
        admit(&db.conn, input("turn-model", parent2, child2, None, "work"))
            .await
            .expect("admit");
        seed_usage(
            &db.conn,
            child2,
            vec![usage_fact("turn-1", Utc::now(), Some("gpt-6-astra"), 10, 1, 0, 0)],
        )
        .await;
        let done = report("turn-model", child2, "done", TaskStatus::Completed);
        assert!(finish(&db.conn, parent2, "turn-model", &done)
            .await
            .expect("finish"));
        assert_eq!(
            metrics_row(&db.conn, "turn-model")
                .await
                .effective_model
                .as_deref(),
            Some("gpt-6-astra")
        );
    }

    /// A failure freezes its `error_code` and nothing it doesn't know.
    #[tokio::test]
    async fn finish_extracts_error_code_for_failures() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("f0", parent, child, None, "work"))
            .await
            .expect("admit");
        let mut failed = report("f0", child, "", TaskStatus::Failed);
        failed.error_code = Some("child_refusal".into());
        failed.text = None;
        failed.duration_ms = Some(90);
        assert!(finish(&db.conn, parent, "f0", &failed)
            .await
            .expect("finish"));
        let row = metrics_row(&db.conn, "f0").await;
        assert_eq!(row.error_code.as_deref(), Some("child_refusal"));
        assert_eq!(row.duration_ms, Some(90));
        assert_eq!(row.turn_count, None);
        assert_eq!(row.input_tokens, None);
    }

    // ── On-demand child sync at freeze time ───────────────────────────────

    /// A Claude-shaped transcript holding one usage-bearing assistant turn.
    fn claude_usage_transcript(
        session_id: &str,
        cwd: &str,
        at: DateTime<Utc>,
        input: u64,
        output: u64,
        cache_creation: u64,
        cache_read: u64,
    ) -> String {
        let stamp = at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        [
            serde_json::json!({
                "type": "user",
                "timestamp": stamp,
                "uuid": "u1",
                "cwd": cwd,
                "sessionId": session_id,
                "message": {"role": "user", "content": [{"type": "text", "text": "work"}]},
            })
            .to_string(),
            serde_json::json!({
                "type": "assistant",
                "timestamp": stamp,
                "uuid": "a1",
                "parentUuid": "u1",
                "cwd": cwd,
                "sessionId": session_id,
                "message": {
                    "id": "msg_01",
                    "role": "assistant",
                    "model": "claude-opus-5",
                    "content": [{"type": "text", "text": "done"}],
                    "usage": {
                        "input_tokens": input,
                        "output_tokens": output,
                        "cache_creation_input_tokens": cache_creation,
                        "cache_read_input_tokens": cache_read,
                    },
                },
            })
            .to_string(),
        ]
        .join("\n")
            + "\n"
    }

    /// Point `agent_type`'s history reads at a tempdir, the way the settings
    /// page stores a relocated home.
    async fn save_agent_env_json(
        db: &crate::db::AppDatabase,
        agent_type: AgentType,
        env_json: &str,
    ) {
        use crate::db::service::agent_setting_service;
        agent_setting_service::ensure_defaults(
            &db.conn,
            &[agent_setting_service::AgentDefaultInput {
                agent_type,
                registry_id: agent_type.to_string(),
                default_sort_order: 0,
            }],
        )
        .await
        .expect("seed agent setting");
        agent_setting_service::update(
            &db.conn,
            agent_type,
            agent_setting_service::AgentSettingsUpdate {
                enabled: true,
                env_json: Some(env_json.to_string()),
                model_provider_id: None,
            },
        )
        .await
        .expect("save env_json");
    }

    /// Write `body` where Claude's parser looks for `session_id`, and create a
    /// conversation bound to it.
    async fn claude_child_with_transcript(
        db: &crate::db::AppDatabase,
        config_dir: &std::path::Path,
        folder: i32,
        session_id: &str,
        body: &str,
    ) -> i32 {
        let project = config_dir.join("projects").join("-codeg-delegation-sync");
        std::fs::create_dir_all(&project).expect("project dir");
        std::fs::write(project.join(format!("{session_id}.jsonl")), body)
            .expect("write transcript");
        let child =
            conversation_service::create(&db.conn, folder, AgentType::ClaudeCode, None, None)
                .await
                .expect("child")
                .id;
        conversation_service::bind_external_id(&db.conn, child, session_id, &[])
            .await
            .expect("bind session");
        child
    }

    /// Pull a task's admission stamp back so a turn written before the test
    /// body runs falls inside the freeze window.
    async fn widen_task_window(db: &crate::db::AppDatabase, task_id: &str, since: DateTime<Utc>) {
        delegation_task::Entity::update_many()
            .col_expr(
                delegation_task::Column::CreatedAt,
                sea_orm::sea_query::Expr::value(since),
            )
            .filter(delegation_task::Column::TaskId.eq(task_id))
            .exec(&db.conn)
            .await
            .expect("widen task window");
    }

    /// The gap this closes: the token pipeline only materializes a child when
    /// the dashboard is opened, so a task finishing before that froze NULL even
    /// though its transcript held the numbers. `finish` must sync the child
    /// itself and then re-read the facts.
    #[tokio::test]
    async fn finish_syncs_the_child_transcript_when_no_facts_exist() {
        let config_dir = tempfile::tempdir().expect("tempdir");
        let db = fresh_in_memory_db().await;
        save_agent_env_json(
            &db,
            AgentType::ClaudeCode,
            &serde_json::json!({ "CLAUDE_CONFIG_DIR": config_dir.path() }).to_string(),
        )
        .await;

        let cwd = "/tmp/codeg-delegation-sync";
        let folder = folder_service::add_folder(&db.conn, cwd)
            .await
            .expect("folder")
            .id;
        let parent =
            conversation_service::create(&db.conn, folder, AgentType::ClaudeCode, None, None)
                .await
                .expect("parent")
                .id;
        let turn_at = Utc::now() - chrono::Duration::seconds(30);
        let session = "d3f1c0de-0000-4000-8000-000000000001";
        let child = claude_child_with_transcript(
            &db,
            config_dir.path(),
            folder,
            session,
            &claude_usage_transcript(session, cwd, turn_at, 120, 7, 30, 40),
        )
        .await;

        admit(&db.conn, input("t0", parent, child, None, "work"))
            .await
            .expect("admit");
        widen_task_window(&db, "t0", turn_at - chrono::Duration::seconds(30)).await;

        let done = report("t0", child, "done", TaskStatus::Completed);
        assert!(finish(&db.conn, parent, "t0", &done).await.expect("finish"));

        let row = metrics_row(&db.conn, "t0").await;
        assert_eq!(row.input_tokens, Some(120));
        assert_eq!(row.output_tokens, Some(7));
        assert_eq!(row.cache_creation_tokens, Some(30));
        assert_eq!(row.cache_read_tokens, Some(40));
        assert_eq!(row.turn_count, Some(1));
    }

    /// Facts already in the table mean the transcript is never touched: the
    /// seeded numbers survive and the transcript's much larger numbers never
    /// land. This is the zero-extra-work guarantee for the common terminal
    /// path.
    #[tokio::test]
    async fn finish_keeps_existing_facts_and_skips_the_sync() {
        let config_dir = tempfile::tempdir().expect("tempdir");
        let db = fresh_in_memory_db().await;
        save_agent_env_json(
            &db,
            AgentType::ClaudeCode,
            &serde_json::json!({ "CLAUDE_CONFIG_DIR": config_dir.path() }).to_string(),
        )
        .await;

        let cwd = "/tmp/codeg-delegation-sync";
        let folder = folder_service::add_folder(&db.conn, cwd)
            .await
            .expect("folder")
            .id;
        let parent =
            conversation_service::create(&db.conn, folder, AgentType::ClaudeCode, None, None)
                .await
                .expect("parent")
                .id;
        let session = "d3f1c0de-0000-4000-8000-000000000002";
        let child = claude_child_with_transcript(
            &db,
            config_dir.path(),
            folder,
            session,
            &claude_usage_transcript(session, cwd, Utc::now(), 9_999, 999, 99, 99),
        )
        .await;

        admit(&db.conn, input("t0", parent, child, None, "work"))
            .await
            .expect("admit");
        seed_usage(
            &db.conn,
            child,
            vec![usage_fact("turn-1", Utc::now(), Some("gpt-6"), 700, 11, 5, 3)],
        )
        .await;

        let done = report("t0", child, "done", TaskStatus::Completed);
        assert!(finish(&db.conn, parent, "t0", &done).await.expect("finish"));

        let row = metrics_row(&db.conn, "t0").await;
        assert_eq!(row.input_tokens, Some(700));
        assert_eq!(row.output_tokens, Some(11));
        assert_eq!(row.cache_creation_tokens, Some(5));
        assert_eq!(row.cache_read_tokens, Some(3));

        // The transcript's single 9 999-token turn never replaced the seeded
        // row: the fact table still holds exactly the seeded facts.
        let stored = crate::db::service::token_usage_service::fetch_facts(
            &db.conn,
            &crate::db::service::token_usage_service::FactQuery::default(),
            100,
        )
        .await
        .expect("stored facts");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].total_tokens, 700 + 11 + 5 + 3);
    }

    /// A sync that fails must not cost the terminal record: `finish` still
    /// succeeds and the four columns stay NULL ("unknown", never 0).
    #[tokio::test]
    async fn a_failed_on_demand_sync_still_freezes_the_terminal_report() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("t0", parent, child, None, "work"))
            .await
            .expect("admit");

        crate::commands::token_usage::fail_next_conversation_parse("synthetic parse failure");

        let done = report("t0", child, "done", TaskStatus::Completed);
        assert!(
            finish(&db.conn, parent, "t0", &done).await.expect("finish"),
            "the terminal write must win even when the sync fails"
        );

        let row = metrics_row(&db.conn, "t0").await;
        assert_eq!(row.status, "completed");
        assert_eq!(row.input_tokens, None);
        assert_eq!(row.output_tokens, None);
        assert_eq!(row.cache_creation_tokens, None);
        assert_eq!(row.cache_read_tokens, None);

        // If `finish` had skipped the sync entirely, the armed failure would
        // still be pending — consuming it proves the on-demand path really ran
        // and handled the error.
        assert!(
            !crate::commands::token_usage::parse_failure_is_pending(),
            "the armed parse failure must have been consumed by finish"
        );
    }

    fn completed_report(task_id: &str, child: i32, duration: u64) -> DelegationTaskReport {
        let mut done = report(task_id, child, "done", TaskStatus::Completed);
        done.duration_ms = Some(duration);
        done
    }

    /// Seed a terminal task and release it so a continuation can be admitted
    /// on top (the rework chain shape).
    async fn finish_and_release(db: &crate::db::AppDatabase, parent: i32, child: i32, id: &str) {
        let duration: u64 = id.bytes().map(|b| (b % 10) as u64 * 100).sum();
        finish(
            &db.conn,
            parent,
            id,
            &completed_report(id, child, duration.max(100)),
        )
        .await
        .expect("finish chain link");
        mark_released(&db.conn, parent, id)
            .await
            .expect("release chain link");
    }

    #[tokio::test]
    async fn performance_report_on_empty_ledger_is_zeroed() {
        let db = fresh_in_memory_db().await;
        let report = performance_report(&db.conn).await.expect("aggregate");
        assert_eq!(report.totals, DelegationDimensionStats::default());
        assert!(report.by_agent.is_empty());
        assert!(report.by_model.is_empty());
    }

    /// One full rework chain (A→B terminal, C running on the same child),
    /// one failed task under a different agent, and one selector-bearing
    /// task: exercises every rate, bucket, and dimension at once.
    ///
    /// Terminal rows: A (completed), B (completed), D (failed) → 3.
    /// Reworked: A and B (C continues B; B continues A) → 2/3.
    /// Success: A and B → 2/3.
    #[tokio::test]
    async fn performance_report_derives_dimensions_and_rework_rates() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;

        // A: completed WITH selectors/tokens (the selector-bearing row).
        admit(&db.conn, input("a", parent, child, None, "chain start"))
            .await
            .expect("admit a");
        let mut done = completed_report("a", child, 1_000);
        done.turn_count = Some(3);
        done.token_usage = Some(TokenUsage {
            input: 1_200,
            output: 340,
        });
        done.selectors = Some(DelegationSelectorReport {
            requested: SelectorPreferences::default(),
            effective: Some(AppliedSelectors {
                mode: Some("full-auto".into()),
                config_values: BTreeMap::from([(String::from("model"), String::from("gpt-6"))]),
            }),
        });
        assert!(finish(&db.conn, parent, "a", &done)
            .await
            .expect("finish a"));
        mark_released(&db.conn, parent, "a")
            .await
            .expect("release a");

        // B: rework of A, completed, no selectors.
        admit(&db.conn, input("b", parent, child, Some("a"), "rework"))
            .await
            .expect("admit b");
        assert!(
            finish(&db.conn, parent, "b", &completed_report("b", child, 2_000))
                .await
                .expect("finish b")
        );
        mark_released(&db.conn, parent, "b")
            .await
            .expect("release b");

        // C: rework of B, still running.
        admit(
            &db.conn,
            input("c", parent, child, Some("b"), "rework again"),
        )
        .await
        .expect("admit c");

        // D: one-shot failure on a ClaudeCode child (same parent, same folder).
        let parent_folder = conversation::Entity::find_by_id(parent)
            .one(&db.conn)
            .await
            .expect("parent row")
            .expect("parent")
            .folder_id;
        let second_child = conversation_service::create(
            &db.conn,
            parent_folder,
            AgentType::ClaudeCode,
            None,
            None,
        )
        .await
        .expect("second child");
        // `input()` hard-codes the Codex binding, so build this one directly.
        admit(
            &db.conn,
            AdmissionInput {
                task_id: "d".into(),
                parent_conversation_id: parent,
                child_conversation_id: second_child.id,
                source_task_id: None,
                task: "one shot".into(),
                requested_working_dir: None,
                resume_binding: ResumeBinding {
                    agent_type: AgentType::ClaudeCode,
                    external_session_id: "session-d".into(),
                    child_conversation_id: second_child.id,
                    working_dir: "/workspace/project".into(),
                    preferred_mode_id: None,
                    preferred_config_values: BTreeMap::new(),
                    config_fingerprint: "fingerprint-d".into(),
                },
                work_task_id: None,
            },
        )
        .await
        .expect("admit d");
        let mut failed = report("d", second_child.id, "", TaskStatus::Failed);
        failed.agent_type = Some(AgentType::ClaudeCode);
        failed.text = None;
        failed.error_code = Some("child_refusal".into());
        failed.duration_ms = Some(500);
        assert!(finish(&db.conn, parent, "d", &failed)
            .await
            .expect("finish d"));

        let report = performance_report(&db.conn).await.expect("aggregate");

        // Totals: 4 rows, 3 terminal, 1 running.
        assert_eq!(report.totals.task_count, 4);
        assert_eq!(report.totals.running, 1);
        assert_eq!(report.totals.completed, 2);
        assert_eq!(report.totals.failed, 1);
        assert_eq!(report.totals.reworked, 2);
        assert!((report.totals.rework_rate - 2.0 / 3.0).abs() < 1e-9);
        assert!((report.totals.success_rate - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(report.totals.input_tokens, 1_200);
        assert_eq!(report.totals.output_tokens, 340);
        let avg = report.totals.avg_duration_ms.expect("avg duration");
        assert!((avg - (1_000.0 + 2_000.0 + 500.0) / 3.0).abs() < 1e-9);

        // By agent: codex (a, b, c) sorted first, claude_code (d) second.
        assert_eq!(report.by_agent.len(), 2);
        assert_eq!(report.by_agent[0].key.as_deref(), Some("codex"));
        assert_eq!(report.by_agent[0].task_count, 3);
        assert_eq!(report.by_agent[0].running, 1);
        // Terminal under codex = {a, b}; both reworked → 1.0.
        assert!((report.by_agent[0].rework_rate - 1.0).abs() < 1e-9);
        assert_eq!(report.by_agent[1].key.as_deref(), Some("claude_code"));
        assert_eq!(report.by_agent[1].failed, 1);
        assert_eq!(report.by_agent[1].reworked, 0);

        // By model: only "a" recorded a model; b, c, d bucket under None.
        assert_eq!(report.by_model.len(), 2);
        assert_eq!(report.by_model[0].key.as_deref(), None);
        assert_eq!(report.by_model[0].task_count, 3);
        assert_eq!(report.by_model[1].key.as_deref(), Some("gpt-6"));
        assert_eq!(report.by_model[1].task_count, 1);
        assert_eq!(report.by_model[1].input_tokens, 1_200);
    }

    /// The dashboard shows the history the user can still open: soft-deleted
    /// children drop out of every bucket.
    #[tokio::test]
    async fn performance_report_excludes_soft_deleted_conversations() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;
        admit(&db.conn, input("t0", parent, child, None, "work"))
            .await
            .expect("admit");
        finish_and_release(&db, parent, child, "t0").await;
        assert_eq!(
            performance_report(&db.conn)
                .await
                .unwrap()
                .totals
                .task_count,
            1
        );

        conversation_service::soft_delete(&db.conn, child)
            .await
            .expect("delete child");
        let report = performance_report(&db.conn).await.expect("aggregate");
        assert_eq!(report.totals.task_count, 0);
        assert!(report.by_agent.is_empty());
    }

    /// Attribution is STORED at admission and read back per task, ordered by
    /// admission: the task detail's "sub-agent runs" ledger.
    #[tokio::test]
    async fn a_work_task_attribution_is_stored_and_listed_for_that_task() {
        let db = fresh_in_memory_db().await;
        let (parent, child) = conversations(&db).await;

        let mut first = input("attributed-1", parent, child, None, "first round");
        first.work_task_id = Some(42);
        assert!(matches!(
            admit(&db.conn, first).await.expect("admit 1"),
            AdmissionResult::New { .. }
        ));

        let mut second = input("attributed-2", parent, child, None, "second round");
        second.work_task_id = Some(42);
        assert!(matches!(
            admit(&db.conn, second).await.expect("admit 2"),
            AdmissionResult::New { .. }
        ));

        // A plain chat delegation stays unattributed.
        assert!(matches!(
            admit(&db.conn, input("unattributed", parent, child, None, "chat"))
                .await
                .expect("admit 3"),
            AdmissionResult::New { .. }
        ));

        let rows = list_for_task(&db.conn, 42).await.expect("list");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].task_id, "attributed-1");
        assert_eq!(rows[1].task_id, "attributed-2");
        assert_eq!(rows[0].status, "running");
        assert_eq!(rows[0].agent_type.as_deref(), Some("codex"));
        assert_eq!(rows[0].child_conversation_id, child);
        assert!(list_for_task(&db.conn, 7).await.expect("other").is_empty());

        // The stored link survives the task's own history: a row keeps its
        // attribution even after a later admission for another task.
        let mut other = input("attributed-3", parent, child, None, "elsewhere");
        other.work_task_id = Some(7);
        admit(&db.conn, other).await.expect("admit 4");
        let rows = list_for_task(&db.conn, 42).await.expect("list");
        assert_eq!(rows.len(), 2, "only the 42 rows belong to task 42");
    }

    /// The wire projection turns the boot sweep's `unknown` into
    /// `interrupted`, and carries the terminal metrics through.
    #[tokio::test]
    async fn list_for_task_projects_interrupted_and_terminal_metrics() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = fresh_disk_db(dir.path()).await;
        let (parent, child) = conversations(&db).await;
        let mut input = input("interrupted-run", parent, child, None, "work");
        input.work_task_id = Some(9);
        admit(&db.conn, input).await.expect("admit");
        boot_reconcile_interrupted(&db.conn)
            .await
            .expect("boot sweep");

        let rows = list_for_task(&db.conn, 9).await.expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "interrupted");
        assert_eq!(rows[0].error_code.as_deref(), Some("interrupted"));
    }

    /// `find_live_by_conversation` is the admission-time resolver: it returns
    /// only the live task that currently owns the delegating conversation.
    #[tokio::test]
    async fn only_the_live_owner_of_a_conversation_is_attributed() {
        use sea_orm::ActiveModelTrait;
        use sea_orm::ActiveValue::{NotSet, Set};

        let db = fresh_in_memory_db().await;
        let (parent, _child) = conversations(&db).await;

        let now = chrono::Utc::now();
        let insert = |title: &str, conversation_id: Option<i32>, deleted: bool| {
            crate::db::entities::work_task::ActiveModel {
                deferred_at: Set(None),
                delivery: Set(None),
                id: NotSet,
                folder_id: Set(1),
                title: Set(title.to_string()),
                config: Set("{}".to_string()),
                status: Set(crate::db::entities::work_task::WorkTaskStatus::Running),
                failure_reason: Set(None),
                last_error: Set(None),
                run_seq: Set(1),
                sort_order: Set(0),
                worktree_folder_id: Set(None),
                conversation_id: Set(conversation_id),
                connection_id: Set(None),
                base_branch: Set(None),
                base_sha: Set(None),
                work_branch: Set(None),
                merge_state: Set(None),
                pending_merge: Set(None),
                cleanup_state: Set(None),
                verdict: Set(None),
                result_summary: Set(None),
                files_changed: Set(None),
                additions: Set(None),
                deletions: Set(None),
                merge_commit: Set(None),
                completion_kind: Set(None),
                preflight: Set(None),
                archived_at: Set(None),
                scheduled_at: Set(None),
                source_kind: Set(None),
                source_key: Set(None),
                source_meta: Set(None),
                parent_id: Set(None),
                created_by_conversation_id: Set(None),
                max_concurrent_children: Set(None),
                max_runs_per_child: Set(None),
                token_budget: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                started_at: Set(None),
                settled_at: Set(None),
                finished_at: Set(None),
                deleted_at: Set(deleted.then_some(now)),
            }
        };

        let live = insert("owner", Some(parent), false)
            .insert(&db.conn)
            .await
            .expect("live task")
            .id;
        assert_eq!(
            crate::db::service::work_task_service::find_live_by_conversation(&db.conn, parent)
                .await
                .expect("resolve"),
            Some(live)
        );
        assert_eq!(
            crate::db::service::work_task_service::find_live_by_conversation(&db.conn, 999)
                .await
                .expect("resolve"),
            None
        );

        // A soft-deleted task is invisible; the newest live one wins.
        insert("gone", Some(parent), true)
            .insert(&db.conn)
            .await
            .expect("deleted task");
        let newest = insert("newest", Some(parent), false)
            .insert(&db.conn)
            .await
            .expect("newest task")
            .id;
        assert_eq!(
            crate::db::service::work_task_service::find_live_by_conversation(&db.conn, parent)
                .await
                .expect("resolve"),
            Some(newest)
        );
    }
}
