use sea_orm_migration::prelude::*;

/// Links a delegation ledger row to the work task whose execution produced it
/// (#731 phase 2).
///
/// The column is resolved once at admission: if the delegating parent
/// conversation is the current conversation of a live work task, that task's
/// id is stored here. Storing it (rather than deriving it later from
/// `work_task.conversation_id`) keeps the link stable when the task's session
/// changes — a fresh-session rework points `work_task.conversation_id` at a new
/// conversation, and historical delegations must not lose their attribution.
///
/// NULL for every ordinary chat delegation, and for rows written before this
/// column existed. No FK: `work_task` soft-deletes, and the ledger must keep
/// its history readable either way (same soft-reference discipline as
/// `work_task.folder_id`).
#[derive(DeriveMigrationName)]
pub struct Migration;

const IDX_DELEGATION_TASK_WORK_TASK: &str = "idx_delegation_task_work_task";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(DelegationTask::Table)
                    .add_column(ColumnDef::new(DelegationTask::WorkTaskId).integer().null())
                    .to_owned(),
            )
            .await?;
        // The task detail's "sub-agent runs" read.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(IDX_DELEGATION_TASK_WORK_TASK)
                    .table(DelegationTask::Table)
                    .col(DelegationTask::WorkTaskId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(IDX_DELEGATION_TASK_WORK_TASK)
                    .table(DelegationTask::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(DelegationTask::Table)
                    .drop_column(DelegationTask::WorkTaskId)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum DelegationTask {
    Table,
    WorkTaskId,
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

    /// Additive column on a populated ledger: existing rows keep their data and
    /// read NULL; new rows can carry a task link and the index serves the
    /// per-task read.
    #[tokio::test]
    async fn adds_work_task_link_without_touching_legacy_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ledger-link.db");
        let url = format!("sqlite:{}?mode=rwc", path.to_string_lossy());
        let conn = Database::connect(url.clone()).await.expect("open db");
        conn.execute(sql("PRAGMA foreign_keys=ON;"))
            .await
            .expect("foreign keys");

        let migrations = <Migrator as MigratorTrait>::migrations();
        let idx = migrations
            .iter()
            .position(|migration| migration.name() == "m20260926_000004_delegation_work_task_link")
            .expect("link migration is registered");
        Migrator::up(&conn, Some(idx as u32))
            .await
            .expect("apply pre-link schema");

        let folder = folder_service::add_folder(&conn, "/workspace/link")
            .await
            .expect("folder");
        let parent =
            conversation_service::create(&conn, folder.id, AgentType::ClaudeCode, None, None)
                .await
                .expect("parent");
        let child = conversation_service::create(&conn, folder.id, AgentType::Codex, None, None)
            .await
            .expect("child");
        conn.execute(sql(&format!(
            "INSERT INTO delegation_task \
             (task_id, parent_conversation_id, child_conversation_id, source_task_id, \
              task, requested_working_dir, status, terminal_report, resume_binding, \
              released, created_at, updated_at) VALUES \
             ('legacy-task', {p}, {c}, NULL, 'work', NULL, \
              'running', NULL, '{{\"agent_type\":\"codex\"}}', 0, \
              '2026-09-01 00:00:00+00:00', '2026-09-01 00:00:00+00:00')",
            p = parent.id,
            c = child.id,
        )))
        .await
        .expect("insert legacy row");

        conn.close().await.expect("close pre-link db");

        let conn = Database::connect(url).await.expect("reopen for upgrade");
        conn.execute(sql("PRAGMA foreign_keys=ON;"))
            .await
            .expect("foreign keys after reopen");
        Migrator::up(&conn, Some(idx as u32 + 1))
            .await
            .expect("apply link migration");

        let column = conn
            .query_one(sql(
                "SELECT COUNT(*) AS n FROM pragma_table_info('delegation_task') \
                 WHERE name = 'work_task_id'",
            ))
            .await
            .expect("probe column")
            .expect("row")
            .try_get::<i64>("", "n")
            .expect("count");
        assert_eq!(column, 1, "work_task_id must exist after upgrade");

        let legacy = conn
            .query_one(sql(
                "SELECT COUNT(*) AS n FROM delegation_task \
                 WHERE task_id = 'legacy-task' AND work_task_id IS NULL",
            ))
            .await
            .expect("probe legacy")
            .expect("row")
            .try_get::<i64>("", "n")
            .expect("count");
        assert_eq!(legacy, 1, "legacy rows must keep NULL attribution");

        // A new row can carry the link, and the index serves the task read.
        conn.execute(sql(
            "INSERT INTO delegation_task \
             (task_id, parent_conversation_id, child_conversation_id, source_task_id, \
              task, requested_working_dir, status, terminal_report, resume_binding, \
              released, work_task_id, created_at, updated_at) VALUES \
             ('linked-task', 1, 2, NULL, 'work', NULL, \
              'running', NULL, '{\"agent_type\":\"codex\"}', 0, 42, \
              '2026-09-01 00:00:00+00:00', '2026-09-01 00:00:00+00:00')",
        ))
        .await
        .expect("insert linked row");
        let linked = conn
            .query_one(sql(
                "SELECT COUNT(*) AS n FROM delegation_task WHERE work_task_id = 42",
            ))
            .await
            .expect("probe link")
            .expect("row")
            .try_get::<i64>("", "n")
            .expect("count");
        assert_eq!(linked, 1, "the task link must round-trip");
        conn.close().await.expect("close upgraded db");
    }
}
