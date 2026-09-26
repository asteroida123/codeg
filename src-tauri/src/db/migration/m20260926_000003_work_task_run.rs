use sea_orm_migration::prelude::*;

/// The durable run ledger of a work task (#731 phase 2): one row per execution
/// generation, holding the session binding a strict continuation resumes plus
/// the per-run cost/outcome record.
///
/// This is the task-side counterpart of `delegation_task`. It is deliberately a
/// separate table: a task run's lifecycle (worktree setup, review/merge
/// generations, `run_seq` CAS) is owned by the task engine, while the
/// delegation ledger's release/routing semantics are driven by the broker and
/// the child connection — forcing the two into one table would make each
/// misread the other. The link between them is
/// `delegation_task.work_task_id` (migration 000004), set when a task's own
/// agent delegates work.
///
/// `run_seq` mirrors `work_task.run_seq`: claims bump it, and merge/delivery
/// dispatch bumps it too, so a task can hold at most one row per generation
/// (enforced by the unique index). Pre-feature history has no rows; the table
/// is append-only from the moment it exists.
#[derive(DeriveMigrationName)]
pub struct Migration;

const IDX_WORK_TASK_RUN_TASK_SEQ: &str = "idx_work_task_run_task_seq";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(WorkTaskRun::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(WorkTaskRun::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(WorkTaskRun::TaskId).integer().not_null())
                    // The generation this run belongs to (`work_task.run_seq`).
                    .col(ColumnDef::new(WorkTaskRun::RunSeq).integer().not_null())
                    // fresh | retry | return | merge — what started this
                    // generation.
                    .col(ColumnDef::new(WorkTaskRun::Kind).string().not_null())
                    // running | settled | failed | canceled. `settled` means
                    // the generation ended and the task moved on (review or
                    // done); it is not a verdict of its own.
                    .col(ColumnDef::new(WorkTaskRun::Status).string().not_null())
                    // How the previous session was (or was not) continued:
                    // resumed | fresh_no_session | fresh_requested |
                    // fallback_cold | strict_failed. NULL on history written
                    // before the column existed.
                    .col(ColumnDef::new(WorkTaskRun::ResumeOutcome).string().null())
                    // The run whose session this one resumed, when it did.
                    .col(
                        ColumnDef::new(WorkTaskRun::ResumedFromRunSeq)
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(WorkTaskRun::AgentType).string().null())
                    .col(ColumnDef::new(WorkTaskRun::ConversationId).integer().null())
                    // The agent-assigned session identity captured for this
                    // run — the strict anchor a continuation resumes. NULL when
                    // the agent never reported one.
                    .col(
                        ColumnDef::new(WorkTaskRun::ExternalSessionId)
                            .text()
                            .null(),
                    )
                    .col(ColumnDef::new(WorkTaskRun::WorkingDir).text().null())
                    // Effective selectors at launch, for the run's record.
                    .col(ColumnDef::new(WorkTaskRun::EffectiveMode).string().null())
                    .col(ColumnDef::new(WorkTaskRun::EffectiveModel).string().null())
                    .col(
                        ColumnDef::new(WorkTaskRun::EffectiveReasoningLevel)
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(WorkTaskRun::StartedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(WorkTaskRun::FinishedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(WorkTaskRun::DurationMs)
                            .big_integer()
                            .null(),
                    )
                    // Token facts folded in at close time (best-effort: the
                    // token-usage pipeline lags and not every agent reports).
                    .col(ColumnDef::new(WorkTaskRun::InputTokens).big_integer().null())
                    .col(
                        ColumnDef::new(WorkTaskRun::OutputTokens)
                            .big_integer()
                            .null(),
                    )
                    .col(ColumnDef::new(WorkTaskRun::Verdict).string().null())
                    // agent_error | setup_error | verdict_blocked |
                    // interrupted | resume_failed
                    .col(ColumnDef::new(WorkTaskRun::ErrorCode).string().null())
                    .col(
                        ColumnDef::new(WorkTaskRun::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(WorkTaskRun::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(WorkTaskRun::Table, WorkTaskRun::TaskId)
                            .to(WorkTask::Table, WorkTask::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    // A physically deleted conversation nulls the pointer
                    // instead of erasing the run: the cost/outcome record is
                    // history and must survive the session it described.
                    .foreign_key(
                        ForeignKey::create()
                            .from(WorkTaskRun::Table, WorkTaskRun::ConversationId)
                            .to(Conversation::Table, Conversation::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // One row per generation, and the list read (per task, newest first)
        // rides the same index.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(IDX_WORK_TASK_RUN_TASK_SEQ)
                    .table(WorkTaskRun::Table)
                    .col(WorkTaskRun::TaskId)
                    .col(WorkTaskRun::RunSeq)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().if_exists().table(WorkTaskRun::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum WorkTaskRun {
    Table,
    Id,
    TaskId,
    RunSeq,
    Kind,
    Status,
    ResumeOutcome,
    ResumedFromRunSeq,
    AgentType,
    ConversationId,
    ExternalSessionId,
    WorkingDir,
    EffectiveMode,
    EffectiveModel,
    EffectiveReasoningLevel,
    StartedAt,
    FinishedAt,
    DurationMs,
    InputTokens,
    OutputTokens,
    Verdict,
    ErrorCode,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum WorkTask {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Conversation {
    Table,
    Id,
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    use sea_orm_migration::MigratorTrait;

    use crate::db::migration::Migrator;
    use crate::db::service::{conversation_service, folder_service};
    use crate::models::AgentType;

    fn sql(statement: &str) -> Statement {
        Statement::from_string(DbBackend::Sqlite, statement.to_owned())
    }

    async fn probe_n(conn: &sea_orm::DatabaseConnection, statement: &str) -> i64 {
        conn.query_one(sql(statement))
            .await
            .expect("probe")
            .expect("row")
            .try_get::<i64>("", "n")
            .expect("count")
    }

    /// The table is new and standalone: the upgrade creates it, one row per
    /// (task, generation) is enforced, and deleting the task (or its
    /// conversation) cannot destroy a sibling's history.
    #[tokio::test]
    async fn creates_run_ledger_with_generation_uniqueness_and_cascades() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("run-ledger.db");
        let url = format!("sqlite:{}?mode=rwc", path.to_string_lossy());
        let conn = Database::connect(url).await.expect("open db");
        conn.execute(sql("PRAGMA foreign_keys=ON;"))
            .await
            .expect("foreign keys");

        let migrations = <Migrator as MigratorTrait>::migrations();
        let idx = migrations
            .iter()
            .position(|migration| migration.name() == "m20260926_000003_work_task_run")
            .expect("run-ledger migration is registered");
        Migrator::up(&conn, Some(idx as u32 + 1))
            .await
            .expect("apply run-ledger migration");

        let folder = folder_service::add_folder(&conn, "/workspace/runs")
            .await
            .expect("folder");
        let conversation =
            conversation_service::create(&conn, folder.id, AgentType::ClaudeCode, None, None)
                .await
                .expect("conversation");
        conn.execute(sql(&format!(
            "INSERT INTO work_task \
             (folder_id, title, config, status, run_seq, sort_order, created_at, updated_at) \
             VALUES ({f}, 'task', '{{}}', 'todo', 0, 0, \
              '2026-09-01 00:00:00+00:00', '2026-09-01 00:00:00+00:00')",
            f = folder.id,
        )))
        .await
        .expect("insert task");

        let run_insert = |run_seq: i32, session: &str| {
            format!(
                "INSERT INTO work_task_run \
                 (task_id, run_seq, kind, status, resume_outcome, agent_type, \
                  conversation_id, external_session_id, started_at, created_at, updated_at) \
                 VALUES (1, {run_seq}, 'fresh', 'running', 'fresh_no_session', 'claude_code', \
                  {c}, '{session}', '2026-09-01 00:00:00+00:00', \
                  '2026-09-01 00:00:00+00:00', '2026-09-01 00:00:00+00:00')",
                c = conversation.id,
            )
        };
        conn.execute(sql(&run_insert(1, "session-a")))
            .await
            .expect("insert run 1");
        let duplicate = conn.execute(sql(&run_insert(1, "session-b"))).await;
        assert!(
            duplicate.is_err(),
            "one row per (task_id, run_seq) must be enforced"
        );
        conn.execute(sql(&run_insert(2, "session-a")))
            .await
            .expect("insert run 2");

        // Physically deleting the conversation keeps the run row (SET NULL).
        conn.execute(sql(&format!(
            "DELETE FROM conversation WHERE id = {}",
            conversation.id
        )))
        .await
        .expect("delete conversation");
        let kept = probe_n(
            &conn,
            "SELECT COUNT(*) AS n FROM work_task_run WHERE conversation_id IS NULL",
        )
        .await;
        assert_eq!(kept, 2, "runs must survive their conversation");

        // Deleting the task takes its runs with it.
        conn.execute(sql("DELETE FROM work_task WHERE id = 1"))
            .await
            .expect("delete task");
        let remaining = probe_n(&conn, "SELECT COUNT(*) AS n FROM work_task_run").await;
        assert_eq!(remaining, 0, "runs must cascade with their task");
        conn.close().await.expect("close db");
    }
}
