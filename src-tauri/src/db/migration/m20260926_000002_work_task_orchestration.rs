use sea_orm_migration::prelude::*;

/// Work-task orchestration (#731 phase 2): split parents, the agent-facing
/// creator link, the dependency graph, and the per-parent concurrency / run /
/// token limits.
///
/// Every column is nullable and the dependency table is new, so an existing
/// board upgrades byte-identically: no parent, no edges, no limits — exactly
/// the pre-split behaviour.
///
/// A split parent is itself a top-level task (the service caps the hierarchy
/// at two levels), so `parent_id` never chains into a tree. The limits live on
/// the parent row and are read there; NULL means "no limit". `max_runs_per_child`
/// bounds each child's generation count, and `token_budget` is checked against
/// the token-usage facts of the parent and its children — both enforced in the
/// claim transaction, never mid-run.
#[derive(DeriveMigrationName)]
pub struct Migration;

const IDX_WORK_TASK_PARENT: &str = "idx_work_task_parent";
const IDX_WORK_TASK_CREATED_BY: &str = "idx_work_task_created_by";
const IDX_WORK_TASK_DEPENDENCY_PAIR: &str = "idx_work_task_dependency_pair";
const IDX_WORK_TASK_DEPENDENCY_ON: &str = "idx_work_task_dependency_on";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One ALTER per column: SQLite's ADD COLUMN takes a single column,
        // matching the one-column-per-alter convention of earlier migrations.
        type Def = fn(WorkTask) -> ColumnDef;
        let columns: [(WorkTask, Def); 5] = [
            // Split parent; NULL = top level.
            (WorkTask::ParentId, |c| {
                ColumnDef::new(c).integer().null().take()
            }),
            // The conversation whose agent created this row through an
            // agent-facing authoring tool. Every later agent operation on the
            // task (split / start / cancel / list) is scoped to it. NULL =
            // human-created, or created before this column existed.
            (WorkTask::CreatedByConversationId, |c| {
                ColumnDef::new(c).integer().null().take()
            }),
            // Per-parent orchestration limits, read off the parent row only.
            // Concurrency counts the parent's active children.
            (WorkTask::MaxConcurrentChildren, |c| {
                ColumnDef::new(c).integer().null().take()
            }),
            // Run-count ceiling per child (counted by `run_seq`).
            (WorkTask::MaxRunsPerChild, |c| {
                ColumnDef::new(c).integer().null().take()
            }),
            // Token ceiling across the parent and all its children.
            (WorkTask::TokenBudget, |c| {
                ColumnDef::new(c).big_integer().null().take()
            }),
        ];
        for (column, def) in columns {
            manager
                .alter_table(
                    Table::alter()
                        .table(WorkTask::Table)
                        .add_column(def(column))
                        .to_owned(),
                )
                .await?;
        }

        // Board grouping and the "children of X" read.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(IDX_WORK_TASK_PARENT)
                    .table(WorkTask::Table)
                    .col(WorkTask::ParentId)
                    .to_owned(),
            )
            .await?;
        // "Tasks created by this conversation" — the agent tool's scope check.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(IDX_WORK_TASK_CREATED_BY)
                    .table(WorkTask::Table)
                    .col(WorkTask::CreatedByConversationId)
                    .to_owned(),
            )
            .await?;

        // work_task_dependency: directed "waits for" edges. Same folder only,
        // cycles rejected by the service. A hard gate: the dependent task is
        // not claimable until every dependency reached `done`. Soft-deleted
        // tasks keep their edges (the column is a soft reference), and the
        // gate treats a deleted dependency as unmet rather than satisfied.
        manager
            .create_table(
                Table::create()
                    .table(WorkTaskDependency::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(WorkTaskDependency::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(WorkTaskDependency::TaskId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(WorkTaskDependency::DependsOnTaskId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(WorkTaskDependency::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(WorkTaskDependency::Table, WorkTaskDependency::TaskId)
                            .to(WorkTask::Table, WorkTask::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(WorkTaskDependency::Table, WorkTaskDependency::DependsOnTaskId)
                            .to(WorkTask::Table, WorkTask::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // One edge per (dependent, dependency) pair — a duplicate would be a
        // silent double count in the gate.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(IDX_WORK_TASK_DEPENDENCY_PAIR)
                    .table(WorkTaskDependency::Table)
                    .col(WorkTaskDependency::TaskId)
                    .col(WorkTaskDependency::DependsOnTaskId)
                    .unique()
                    .to_owned(),
            )
            .await?;
        // Reverse lookup: "who is waiting on me", used by the board's unblock
        // sweep and by the delete/remove paths.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(IDX_WORK_TASK_DEPENDENCY_ON)
                    .table(WorkTaskDependency::Table)
                    .col(WorkTaskDependency::DependsOnTaskId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .if_exists()
                    .table(WorkTaskDependency::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(IDX_WORK_TASK_CREATED_BY)
                    .table(WorkTask::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(IDX_WORK_TASK_PARENT)
                    .table(WorkTask::Table)
                    .to_owned(),
            )
            .await?;
        for column in [
            WorkTask::TokenBudget,
            WorkTask::MaxRunsPerChild,
            WorkTask::MaxConcurrentChildren,
            WorkTask::CreatedByConversationId,
            WorkTask::ParentId,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(WorkTask::Table)
                        .drop_column(column)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum WorkTask {
    Table,
    Id,
    ParentId,
    CreatedByConversationId,
    MaxConcurrentChildren,
    MaxRunsPerChild,
    TokenBudget,
}

#[derive(DeriveIden)]
enum WorkTaskDependency {
    Table,
    Id,
    TaskId,
    DependsOnTaskId,
    CreatedAt,
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

    /// Additive upgrade with history: a pre-orchestration task row keeps its
    /// data and reads NULL orchestration columns; the new table and indexes
    /// exist afterwards; duplicate edges are rejected by the unique index.
    #[tokio::test]
    async fn adds_orchestration_columns_and_dependency_table_without_touching_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pre-orchestration.db");
        let url = format!("sqlite:{}?mode=rwc", path.to_string_lossy());
        let conn = Database::connect(url.clone()).await.expect("open db");
        conn.execute(sql("PRAGMA foreign_keys=ON;"))
            .await
            .expect("foreign keys");

        let migrations = <Migrator as MigratorTrait>::migrations();
        let idx = migrations
            .iter()
            .position(|migration| migration.name() == "m20260926_000002_work_task_orchestration")
            .expect("orchestration migration is registered");
        Migrator::up(&conn, Some(idx as u32))
            .await
            .expect("apply pre-orchestration schema");

        // One legacy task, written the way the pre-orchestration binary did.
        let folder = folder_service::add_folder(&conn, "/workspace/legacy")
            .await
            .expect("folder");
        let conversation =
            conversation_service::create(&conn, folder.id, AgentType::ClaudeCode, None, None)
                .await
                .expect("conversation");
        conn.execute(sql(&format!(
            "INSERT INTO work_task \
             (folder_id, title, config, status, run_seq, sort_order, created_at, updated_at) \
             VALUES ({f}, 'legacy task', '{{}}', 'todo', 0, 0, \
              '2026-09-01 00:00:00+00:00', '2026-09-01 00:00:00+00:00')",
            f = folder.id,
        )))
        .await
        .expect("insert legacy row");

        conn.close().await.expect("close pre-orchestration db");

        // Reopen so no connection-local schema cache makes the upgrade easier
        // than a real application restart.
        let conn = Database::connect(url).await.expect("reopen for upgrade");
        conn.execute(sql("PRAGMA foreign_keys=ON;"))
            .await
            .expect("foreign keys after reopen");
        Migrator::up(&conn, Some(idx as u32 + 1))
            .await
            .expect("apply orchestration migration");

        for expected in [
            "parent_id",
            "created_by_conversation_id",
            "max_concurrent_children",
            "max_runs_per_child",
            "token_budget",
        ] {
            let found = probe_n(
                &conn,
                &format!(
                    "SELECT COUNT(*) AS n FROM pragma_table_info('work_task') \
                     WHERE name = '{expected}'"
                ),
            )
            .await;
            assert_eq!(found, 1, "column {expected} must exist after upgrade");
        }
        let legacy_untouched = probe_n(
            &conn,
            "SELECT COUNT(*) AS n FROM work_task WHERE title = 'legacy task' \
             AND parent_id IS NULL AND created_by_conversation_id IS NULL \
             AND max_concurrent_children IS NULL AND max_runs_per_child IS NULL \
             AND token_budget IS NULL",
        )
        .await;
        assert_eq!(legacy_untouched, 1, "the upgrade must not invent limits");

        let table_exists = probe_n(
            &conn,
            "SELECT COUNT(*) AS n FROM sqlite_master \
             WHERE type = 'table' AND name = 'work_task_dependency'",
        )
        .await;
        assert_eq!(table_exists, 1, "dependency table must exist");

        // The FK path is real: two tasks, one edge, and a duplicate rejected.
        conn.execute(sql(&format!(
            "INSERT INTO work_task \
             (folder_id, title, config, status, run_seq, sort_order, created_at, updated_at) \
             VALUES ({f}, 'child', '{{}}', 'todo', 0, 1, \
              '2026-09-01 00:00:00+00:00', '2026-09-01 00:00:00+00:00')",
            f = folder.id,
        )))
        .await
        .expect("insert child");
        conn.execute(sql(
            "INSERT INTO work_task_dependency (task_id, depends_on_task_id, created_at) \
             VALUES (2, 1, '2026-09-01 00:00:00+00:00')",
        ))
        .await
        .expect("insert edge");
        let duplicate = conn
            .execute(sql(
                "INSERT INTO work_task_dependency (task_id, depends_on_task_id, created_at) \
                 VALUES (2, 1, '2026-09-01 00:00:00+00:00')",
            ))
            .await;
        assert!(
            duplicate.is_err(),
            "the unique pair index must reject a duplicate edge"
        );

        // Deleting the dependent task cascades its edges away.
        conn.execute(sql("DELETE FROM work_task WHERE id = 2"))
            .await
            .expect("delete child");
        let edges = probe_n(&conn, "SELECT COUNT(*) AS n FROM work_task_dependency")
            .await;
        assert_eq!(edges, 0, "edges must cascade with their task");

        // The conversation probe keeps the fixture honest (no unused warnings).
        assert!(conversation.id > 0);
        conn.close().await.expect("close upgraded db");
    }
}
