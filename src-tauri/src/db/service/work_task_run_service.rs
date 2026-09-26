//! Durable per-generation run ledger of a work task (#731 phase 2).
//!
//! One row per execution generation, written by the task engine:
//! - opened when a launch owns its generation (right after `begin_setup`
//!   succeeds, or for a merge generation right after `begin_merge` won its CAS),
//! - closed on EVERY exit path — a launch that unwinds before its prompt, a
//!   canceled generation, a settled turn, an agent failure, a lost worker
//!   (reconcile) and the boot sweep that fails interrupted tasks.
//!
//! The write helpers are all CAS-shaped and best-effort by design: closing a
//! row that is no longer `running` is a no-op, so overlapping settle paths
//! (a late `TurnComplete` after a cancel, reconcile racing the event loop)
//! cannot overwrite the first, authoritative outcome. The ledger never gates
//! the state machine — it records it.
//!
//! `resume_anchor` is the read that makes strict continuation possible: the
//! most recent generation that recorded an agent session AND whose
//! conversation still carries that same external id. Everything else is a
//! read for the task detail's "rounds" list.

use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, NotSet, QueryFilter, QueryOrder, Set,
};

use crate::db::entities::{conversation, work_task, work_task_run};
use crate::db::error::DbError;
use crate::db::service::token_usage_service;
use crate::models::WorkTaskRunInfo;

/// `work_task_run.kind` — what started this generation.
pub const KIND_FRESH: &str = "fresh";
pub const KIND_RETRY: &str = "retry";
pub const KIND_RETURN: &str = "return";
pub const KIND_MERGE: &str = "merge";

/// `work_task_run.status` — where the generation stands. `settled` means it
/// ended and the task moved on (review or done); it is not a verdict.
pub const STATUS_RUNNING: &str = "running";
pub const STATUS_SETTLED: &str = "settled";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_CANCELED: &str = "canceled";

/// How the previous session was (or was not) continued.
pub const RESUME_RESUMED: &str = "resumed";
/// No session was ever reported for this task — a first run.
pub const RESUME_FRESH_NO_SESSION: &str = "fresh_no_session";
/// The user explicitly asked for a new session.
pub const RESUME_FRESH_REQUESTED: &str = "fresh_requested";
/// Best-effort resume fell back to a cold session (merge generations only).
pub const RESUME_FALLBACK_COLD: &str = "fallback_cold";
/// A strict continuation was refused; the run did not start and no cold
/// session was created.
pub const RESUME_STRICT_FAILED: &str = "strict_failed";

/// `work_task_run.error_code`.
pub const ERROR_AGENT_ERROR: &str = "agent_error";
pub const ERROR_SETUP_ERROR: &str = "setup_error";
pub const ERROR_VERDICT_BLOCKED: &str = "verdict_blocked";
pub const ERROR_INTERRUPTED: &str = "interrupted";
pub const ERROR_RESUME_FAILED: &str = "resume_failed";

/// Everything known when a generation's row is opened. The session identity
/// (`conversation_id` / `external_session_id`) may still be a projection of
/// the anchor here; [`close`] refreshes it from the conversation the run
/// actually bound.
#[derive(Debug, Clone)]
pub struct RunOpen<'a> {
    pub task_id: i32,
    pub run_seq: i32,
    /// [`KIND_FRESH`] | [`KIND_RETRY`] | [`KIND_RETURN`] | [`KIND_MERGE`].
    pub kind: &'a str,
    /// [`RESUME_*`] — the planned outcome. A strict failure is recorded by
    /// [`close`] instead, because it is only known once the spawn refused.
    pub resume_outcome: Option<&'a str>,
    /// The run whose session this one plans to continue.
    pub resumed_from_run_seq: Option<i32>,
    pub agent_type: Option<&'a str>,
    pub conversation_id: Option<i32>,
    pub external_session_id: Option<&'a str>,
    pub working_dir: Option<&'a str>,
    pub effective_mode: Option<&'a str>,
    pub effective_model: Option<&'a str>,
    pub effective_reasoning_level: Option<&'a str>,
}

/// The continuation anchor a retry/return launches from: the run whose session
/// is being continued, the conversation that owns that session, and the
/// external session id itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeAnchor {
    /// `None` for pre-feature history: the task has no run rows at all, and
    /// its live conversation is the only anchor there is.
    pub resumed_from_run_seq: Option<i32>,
    pub conversation_id: i32,
    pub external_session_id: String,
}

/// Open the generation's row. Idempotent per `(task_id, run_seq)`: a second
/// launch of the same generation (or a retried insert after a busy DB) keeps
/// the first row rather than minting a duplicate or failing the launch.
/// Returns `true` when this call inserted the row.
pub async fn open(conn: &DatabaseConnection, run: RunOpen<'_>) -> Result<bool, DbError> {
    let now = Utc::now();
    let active = work_task_run::ActiveModel {
        id: NotSet,
        task_id: Set(run.task_id),
        run_seq: Set(run.run_seq),
        kind: Set(run.kind.to_string()),
        status: Set(STATUS_RUNNING.to_string()),
        resume_outcome: Set(run.resume_outcome.map(str::to_string)),
        resumed_from_run_seq: Set(run.resumed_from_run_seq),
        agent_type: Set(run.agent_type.map(str::to_string)),
        conversation_id: Set(run.conversation_id),
        external_session_id: Set(run.external_session_id.map(str::to_string)),
        working_dir: Set(run.working_dir.map(str::to_string)),
        effective_mode: Set(run.effective_mode.map(str::to_string)),
        effective_model: Set(run.effective_model.map(str::to_string)),
        effective_reasoning_level: Set(run.effective_reasoning_level.map(str::to_string)),
        started_at: Set(now),
        finished_at: Set(None),
        duration_ms: Set(None),
        input_tokens: Set(None),
        output_tokens: Set(None),
        verdict: Set(None),
        error_code: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    let inserted = work_task_run::Entity::insert(active)
        .on_conflict(
            OnConflict::columns([work_task_run::Column::TaskId, work_task_run::Column::RunSeq])
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(conn)
        .await?;
    Ok(inserted == 1)
}

/// Point the generation's row at the session it actually bound — the
/// fresh-session arm creates the conversation after the spawn, and a merge
/// fallback moves to a new conversation mid-launch. The worktree path rides
/// along because it is only known at the same moment (the run row opens before
/// the worktree exists).
pub async fn bind_session(
    conn: &DatabaseConnection,
    task_id: i32,
    run_seq: i32,
    conversation_id: i32,
    working_dir: &str,
) -> Result<bool, DbError> {
    let res = work_task_run::Entity::update_many()
        .col_expr(
            work_task_run::Column::ConversationId,
            sea_orm::sea_query::Expr::value(Some(conversation_id)),
        )
        .col_expr(
            work_task_run::Column::WorkingDir,
            sea_orm::sea_query::Expr::value(Some(working_dir.to_string())),
        )
        .col_expr(
            work_task_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(Utc::now()),
        )
        .filter(work_task_run::Column::TaskId.eq(task_id))
        .filter(work_task_run::Column::RunSeq.eq(run_seq))
        .filter(work_task_run::Column::Status.eq(STATUS_RUNNING))
        .exec(conn)
        .await?;
    Ok(res.rows_affected == 1)
}

/// Rewrite the planned resume outcome once the spawn proved what actually
/// happened (a merge's best-effort fallback to a cold session).
pub async fn set_resume_outcome(
    conn: &DatabaseConnection,
    task_id: i32,
    run_seq: i32,
    resume_outcome: &str,
) -> Result<bool, DbError> {
    let res = work_task_run::Entity::update_many()
        .col_expr(
            work_task_run::Column::ResumeOutcome,
            sea_orm::sea_query::Expr::value(Some(resume_outcome.to_string())),
        )
        .col_expr(
            work_task_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(Utc::now()),
        )
        .filter(work_task_run::Column::TaskId.eq(task_id))
        .filter(work_task_run::Column::RunSeq.eq(run_seq))
        .filter(work_task_run::Column::Status.eq(STATUS_RUNNING))
        .exec(conn)
        .await?;
    Ok(res.rows_affected == 1)
}

/// Close the generation's row. First terminal write wins: a row that is no
/// longer `running` is left exactly as it was, which is what makes every
/// settle path (turn complete, cancel, reconcile, boot) safe to call
/// unconditionally.
///
/// The close folds in the run's cost record and freezes its session anchor:
/// - duration from the row's own `started_at`;
/// - the conversation's token total, best-effort (the token pipeline lags and
///   not every agent reports, so only a positive total is recorded and NULL
///   means "not known", never "zero");
/// - `external_session_id` re-read from the conversation, because a FRESH
///   session only learns its agent-assigned id after the spawn returns.
pub async fn close(
    conn: &DatabaseConnection,
    task_id: i32,
    run_seq: i32,
    status: &str,
    error_code: Option<&str>,
    verdict: Option<&str>,
    resume_outcome: Option<&str>,
) -> Result<bool, DbError> {
    let Some(row) = find_run(conn, task_id, run_seq).await? else {
        return Ok(false);
    };
    if row.status != STATUS_RUNNING {
        return Ok(false);
    }
    let now = Utc::now();
    let duration_ms = (now - row.started_at).num_milliseconds().max(0);

    let (tokens, external_session_id) = match row.conversation_id {
        Some(conversation_id) => {
            let tokens = match token_usage_service::total_tokens_for_conversations(
                conn,
                &[conversation_id],
            )
            .await
            {
                Ok(total) if total > 0 => Some(total),
                Ok(_) => None,
                Err(e) => {
                    tracing::warn!(
                        "[work_task] run {task_id}/{run_seq}: token facts unavailable: {e}"
                    );
                    None
                }
            };
            let session = conversation::Entity::find_by_id(conversation_id)
                .one(conn)
                .await?
                .and_then(|c| c.external_id);
            (tokens, session.or(row.external_session_id.clone()))
        }
        None => (None, row.external_session_id.clone()),
    };

    let mut update = work_task_run::Entity::update_many()
        .col_expr(
            work_task_run::Column::Status,
            sea_orm::sea_query::Expr::value(status.to_string()),
        )
        .col_expr(
            work_task_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            work_task_run::Column::DurationMs,
            sea_orm::sea_query::Expr::value(Some(duration_ms)),
        )
        .col_expr(
            work_task_run::Column::InputTokens,
            sea_orm::sea_query::Expr::value(tokens),
        )
        .col_expr(
            work_task_run::Column::ExternalSessionId,
            sea_orm::sea_query::Expr::value(external_session_id),
        )
        .col_expr(
            work_task_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(work_task_run::Column::TaskId.eq(task_id))
        .filter(work_task_run::Column::RunSeq.eq(run_seq))
        // The CAS: only the first close wins.
        .filter(work_task_run::Column::Status.eq(STATUS_RUNNING));
    if let Some(error_code) = error_code {
        update = update.col_expr(
            work_task_run::Column::ErrorCode,
            sea_orm::sea_query::Expr::value(Some(error_code.to_string())),
        );
    }
    if let Some(verdict) = verdict {
        update = update.col_expr(
            work_task_run::Column::Verdict,
            sea_orm::sea_query::Expr::value(Some(verdict.to_string())),
        );
    }
    if let Some(resume_outcome) = resume_outcome {
        update = update.col_expr(
            work_task_run::Column::ResumeOutcome,
            sea_orm::sea_query::Expr::value(Some(resume_outcome.to_string())),
        );
    }
    let res = update.exec(conn).await?;
    Ok(res.rows_affected == 1)
}

/// Close every orphaned `running` row as an interruption.
///
/// An orphan is a row whose task is no longer in an active status: the process
/// that owned the generation is gone, and the boot sweep has already failed
/// (or settled) the task. Rows of tasks that are STILL active — a `merging`
/// task recovering from git truth, say — are deliberately left alone: their
/// settle path closes them from real evidence.
pub async fn close_orphan_running(conn: &DatabaseConnection) -> Result<u64, DbError> {
    let rows = work_task_run::Entity::find()
        .filter(work_task_run::Column::Status.eq(STATUS_RUNNING))
        .all(conn)
        .await?;
    let mut closed = 0;
    for row in rows {
        let task = work_task::Entity::find_by_id(row.task_id).one(conn).await?;
        let still_active = task.is_some_and(|t| {
            matches!(
                t.status,
                crate::db::entities::work_task::WorkTaskStatus::Queued
                    | crate::db::entities::work_task::WorkTaskStatus::Preparing
                    | crate::db::entities::work_task::WorkTaskStatus::Running
                    | crate::db::entities::work_task::WorkTaskStatus::AwaitingInput
                    | crate::db::entities::work_task::WorkTaskStatus::Merging
            )
        });
        if still_active {
            continue;
        }
        if close(
            conn,
            row.task_id,
            row.run_seq,
            STATUS_FAILED,
            Some(ERROR_INTERRUPTED),
            None,
            None,
        )
        .await?
        {
            closed += 1;
        }
    }
    Ok(closed)
}

/// The task detail's "rounds" list: newest generation first.
pub async fn list_for_task(
    conn: &DatabaseConnection,
    task_id: i32,
) -> Result<Vec<WorkTaskRunInfo>, DbError> {
    let rows = work_task_run::Entity::find()
        .filter(work_task_run::Column::TaskId.eq(task_id))
        .order_by_desc(work_task_run::Column::RunSeq)
        .all(conn)
        .await?;
    Ok(rows.into_iter().map(to_info).collect())
}

fn to_info(row: work_task_run::Model) -> WorkTaskRunInfo {
    WorkTaskRunInfo {
        id: row.id,
        task_id: row.task_id,
        run_seq: row.run_seq,
        kind: row.kind,
        status: row.status,
        resume_outcome: row.resume_outcome,
        resumed_from_run_seq: row.resumed_from_run_seq,
        agent_type: row.agent_type,
        conversation_id: row.conversation_id,
        external_session_id: row.external_session_id,
        working_dir: row.working_dir,
        effective_mode: row.effective_mode,
        effective_model: row.effective_model,
        effective_reasoning_level: row.effective_reasoning_level,
        started_at: row.started_at,
        finished_at: row.finished_at,
        duration_ms: row.duration_ms,
        input_tokens: row.input_tokens,
        output_tokens: row.output_tokens,
        verdict: row.verdict,
        error_code: row.error_code,
    }
}

async fn find_run(
    conn: &DatabaseConnection,
    task_id: i32,
    run_seq: i32,
) -> Result<Option<work_task_run::Model>, DbError> {
    Ok(work_task_run::Entity::find()
        .filter(work_task_run::Column::TaskId.eq(task_id))
        .filter(work_task_run::Column::RunSeq.eq(run_seq))
        .one(conn)
        .await?)
}

/// Resolve the session a Retry/Return (or a merge) should continue.
///
/// The anchor is the most recent generation that recorded an
/// `external_session_id` whose conversation row is still live AND still
/// carries exactly that external id. A recorded id the conversation has since
/// moved away from is not an anchor — resuming it would ask the agent for a
/// session it already replaced.
///
/// `task_conversation_id` is the pre-feature fallback: a task that predates
/// this ledger has no run rows at all, and its live conversation (when it has
/// one) is the only continuation evidence there is. That preserves the
/// behaviour those tasks had before the ledger existed instead of silently
/// cold-starting them.
pub async fn resolve_resume_anchor(
    conn: &DatabaseConnection,
    task_id: i32,
    task_conversation_id: Option<i32>,
) -> Result<Option<ResumeAnchor>, DbError> {
    let rows = work_task_run::Entity::find()
        .filter(work_task_run::Column::TaskId.eq(task_id))
        .filter(work_task_run::Column::ExternalSessionId.is_not_null())
        .filter(work_task_run::Column::ConversationId.is_not_null())
        .order_by_desc(work_task_run::Column::RunSeq)
        .all(conn)
        .await?;
    let rows_empty = rows.is_empty();
    for row in rows {
        let Some(conversation_id) = row.conversation_id else {
            continue;
        };
        let Some(recorded) = row.external_session_id.as_deref() else {
            continue;
        };
        let Some(session_id) = live_external_id(conn, conversation_id).await? else {
            continue;
        };
        if session_id != recorded {
            continue;
        }
        return Ok(Some(ResumeAnchor {
            resumed_from_run_seq: Some(row.run_seq),
            conversation_id,
            external_session_id: session_id,
        }));
    }
    // Pre-feature history: no run row exists at all, so the task's live
    // conversation is the anchor.
    if rows_empty {
        if let Some(conversation_id) = task_conversation_id {
            if let Some(session_id) = live_external_id(conn, conversation_id).await? {
                return Ok(Some(ResumeAnchor {
                    resumed_from_run_seq: None,
                    conversation_id,
                    external_session_id: session_id,
                }));
            }
        }
    }
    Ok(None)
}

/// A conversation's agent-assigned session id, or `None` when the conversation
/// is gone or never reported one.
async fn live_external_id(
    conn: &DatabaseConnection,
    conversation_id: i32,
) -> Result<Option<String>, DbError> {
    Ok(conversation::Entity::find_by_id(conversation_id)
        .filter(conversation::Column::DeletedAt.is_null())
        .one(conn)
        .await?
        .and_then(|c| c.external_id))
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::entities::token_usage_turn;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::models::{AgentType, WorkTaskDraft, WorkTaskStatus};
    use sea_orm::sea_query::Expr;
    use sea_orm::{ActiveModelTrait, ActiveValue::NotSet as AmNotSet};

    /// Stand in for the ACP lifecycle's `SessionStarted` write.
    async fn bind_conversation_session(
        db: &crate::db::AppDatabase,
        conversation_id: i32,
        session_id: &str,
    ) {
        conversation::Entity::update_many()
            .col_expr(
                conversation::Column::ExternalId,
                Expr::value(Some(session_id.to_string())),
            )
            .filter(conversation::Column::Id.eq(conversation_id))
            .exec(&db.conn)
            .await
            .expect("session binding");
    }

    async fn seed_task(db: &crate::db::AppDatabase) -> (i32, i32) {
        let folder_id = seed_folder(db, "/tmp/run-ledger").await;
        let conversation_id = seed_conversation(db, folder_id, AgentType::ClaudeCode).await;
        let task = crate::db::service::work_task_service::create(
            &db.conn,
            WorkTaskDraft {
                folder_id,
                title: "ledger".to_string(),
                config: serde_json::json!({ "display_text": "ledger" }),
            },
        )
        .await
        .expect("task");
        (task.id, conversation_id)
    }

    fn open_input(task_id: i32, run_seq: i32) -> RunOpen<'static> {
        RunOpen {
            task_id,
            run_seq,
            kind: KIND_FRESH,
            resume_outcome: Some(RESUME_FRESH_NO_SESSION),
            resumed_from_run_seq: None,
            agent_type: Some("claude_code"),
            conversation_id: None,
            external_session_id: None,
            working_dir: None,
            effective_mode: None,
            effective_model: None,
            effective_reasoning_level: None,
        }
    }

    /// One row per generation: a second open of the same generation is a
    /// no-op, and the row records the session the run bound.
    #[tokio::test]
    async fn one_row_per_generation_and_the_session_binding_round_trips() {
        let db = fresh_in_memory_db().await;
        let (task_id, conversation_id) = seed_task(&db).await;

        assert!(open(&db.conn, open_input(task_id, 0)).await.expect("open"));
        assert!(
            !open(&db.conn, open_input(task_id, 0)).await.expect("re-open"),
            "a second open of the same generation must keep the first row"
        );
        assert!(open(&db.conn, open_input(task_id, 1)).await.expect("open 1"));

        assert!(bind_session(&db.conn, task_id, 0, conversation_id, "/tmp/wt")
            .await
            .expect("bind"));
        assert!(close(
            &db.conn,
            task_id,
            0,
            STATUS_SETTLED,
            None,
            Some("success"),
            None,
        )
        .await
        .expect("close"));

        let runs = list_for_task(&db.conn, task_id).await.expect("list");
        assert_eq!(runs.len(), 2);
        // Newest first.
        assert_eq!(runs[0].run_seq, 1);
        assert_eq!(runs[0].status, STATUS_RUNNING);
        assert_eq!(runs[1].run_seq, 0);
        assert_eq!(runs[1].status, STATUS_SETTLED);
        assert_eq!(runs[1].verdict.as_deref(), Some("success"));
        assert_eq!(runs[1].conversation_id, Some(conversation_id));
        assert_eq!(runs[1].working_dir.as_deref(), Some("/tmp/wt"));
        assert!(runs[1].duration_ms.is_some());
        assert!(runs[1].finished_at.is_some());
    }

    /// Closing is first-writer-wins: a late settle for a canceled generation
    /// cannot rewrite the record, and a second close reports false.
    #[tokio::test]
    async fn close_is_a_first_writer_wins_cas() {
        let db = fresh_in_memory_db().await;
        let (task_id, _) = seed_task(&db).await;
        assert!(open(&db.conn, open_input(task_id, 0)).await.expect("open"));

        assert!(close(
            &db.conn,
            task_id,
            0,
            STATUS_CANCELED,
            None,
            None,
            None
        )
        .await
        .expect("cancel close"));
        assert!(
            !close(
                &db.conn,
                task_id,
                0,
                STATUS_FAILED,
                Some(ERROR_AGENT_ERROR),
                None,
                None,
            )
            .await
            .expect("late close"),
            "the terminal write must not be revisited"
        );
        let runs = list_for_task(&db.conn, task_id).await.expect("list");
        assert_eq!(runs[0].status, STATUS_CANCELED);
        assert_eq!(runs[0].error_code, None);
    }

    /// A close folds the conversation's token facts in, and a launch that
    /// never bound a conversation leaves them unknown rather than zero.
    #[tokio::test]
    async fn close_folds_the_conversations_token_facts() {
        let db = fresh_in_memory_db().await;
        let (task_id, conversation_id) = seed_task(&db).await;
        let now = Utc::now();
        token_usage_turn::ActiveModel {
            id: AmNotSet,
            conversation_id: Set(conversation_id),
            turn_key: Set("turn-1".into()),
            occurred_at: Set(now),
            model: Set(Some("claude-sonnet".into())),
            input_tokens: Set(11),
            output_tokens: Set(7),
            cache_creation_tokens: Set(0),
            cache_read_tokens: Set(0),
            total_tokens: Set(18),
            duration_ms: Set(5),
        }
        .insert(&db.conn)
        .await
        .expect("usage fact");

        assert!(open(&db.conn, open_input(task_id, 0)).await.expect("open"));
        assert!(bind_session(&db.conn, task_id, 0, conversation_id, "/tmp/wt")
            .await
            .expect("bind"));
        assert!(close(
            &db.conn,
            task_id,
            0,
            STATUS_SETTLED,
            None,
            None,
            None
        )
        .await
        .expect("close"));
        let runs = list_for_task(&db.conn, task_id).await.expect("list");
        assert_eq!(runs[0].input_tokens, Some(18));
        assert_eq!(runs[0].output_tokens, None);

        // A generation with no conversation records no token facts.
        assert!(open(&db.conn, open_input(task_id, 1)).await.expect("open"));
        assert!(close(
            &db.conn,
            task_id,
            1,
            STATUS_FAILED,
            Some(ERROR_SETUP_ERROR),
            None,
            None
        )
        .await
        .expect("close"));
        let runs = list_for_task(&db.conn, task_id).await.expect("list");
        assert_eq!(runs[0].input_tokens, None);
        assert_eq!(runs[0].error_code.as_deref(), Some(ERROR_SETUP_ERROR));
    }

    /// The anchor only accepts a session the conversation still carries: a
    /// recorded id the conversation has moved away from is not a resume point.
    #[tokio::test]
    async fn the_resume_anchor_requires_the_live_recorded_session() {
        let db = fresh_in_memory_db().await;
        let (task_id, conversation_id) = seed_task(&db).await;

        let mut input = open_input(task_id, 0);
        input.conversation_id = Some(conversation_id);
        input.external_session_id = Some("session-a");
        assert!(open(&db.conn, input).await.expect("open"));
        assert!(close(
            &db.conn,
            task_id,
            0,
            STATUS_SETTLED,
            None,
            None,
            None
        )
        .await
        .expect("close"));

        // The conversation has not reported the session yet: no anchor.
        assert_eq!(
            resolve_resume_anchor(&db.conn, task_id, None)
                .await
                .expect("anchor"),
            None
        );

        // Once it carries the recorded id, that run is the anchor.
        bind_conversation_session(&db, conversation_id, "session-a").await;
        let anchor = resolve_resume_anchor(&db.conn, task_id, None)
            .await
            .expect("anchor")
            .expect("anchor must exist");
        assert_eq!(anchor.resumed_from_run_seq, Some(0));
        assert_eq!(anchor.conversation_id, conversation_id);
        assert_eq!(anchor.external_session_id, "session-a");

        // The agent replaced the session: the recorded run is no longer a
        // valid anchor, and there is no older one to fall back to.
        bind_conversation_session(&db, conversation_id, "session-b").await;
        assert_eq!(
            resolve_resume_anchor(&db.conn, task_id, None)
                .await
                .expect("anchor"),
            None
        );

        // A task with NO run rows at all (pre-feature history) falls back to
        // its live conversation.
        let (legacy_task, legacy_conversation) = seed_task(&db).await;
        bind_conversation_session(&db, legacy_conversation, "session-legacy").await;
        let anchor = resolve_resume_anchor(&db.conn, legacy_task, Some(legacy_conversation))
            .await
            .expect("anchor")
            .expect("legacy anchor");
        assert_eq!(anchor.resumed_from_run_seq, None);
        assert_eq!(anchor.external_session_id, "session-legacy");
    }

    /// The boot sweep closes only rows whose task is no longer active: a task
    /// still queued / running / merging owns its generation, and its own
    /// settle path closes the row from real evidence.
    #[tokio::test]
    async fn orphan_running_rows_close_with_their_task_and_leave_active_ones() {
        use crate::db::service::work_task_service;
        let db = fresh_in_memory_db().await;
        let (task_id, _) = seed_task(&db).await;
        let run_seq = work_task_service::claim_for_run(
            &db.conn,
            task_id,
            WorkTaskStatus::Todo,
            "test",
        )
        .await
        .expect("claim")
        .expect("claimed");
        assert!(open(&db.conn, open_input(task_id, run_seq))
            .await
            .expect("open"));
        assert_eq!(
            close_orphan_running(&db.conn).await.expect("sweep"),
            0,
            "a queued task still owns its run"
        );

        // The boot sweep fails the task; its run row is now an orphan.
        assert!(work_task_service::fail(
            &db.conn,
            task_id,
            &[WorkTaskStatus::Queued],
            None,
            "interrupted",
            None,
        )
        .await
        .expect("fail"));
        assert_eq!(close_orphan_running(&db.conn).await.expect("sweep"), 1);
        let runs = list_for_task(&db.conn, task_id).await.expect("list");
        assert_eq!(runs[0].status, STATUS_FAILED);
        assert_eq!(runs[0].error_code.as_deref(), Some(ERROR_INTERRUPTED));
        // Idempotent.
        assert_eq!(close_orphan_running(&db.conn).await.expect("sweep"), 0);
    }
}
