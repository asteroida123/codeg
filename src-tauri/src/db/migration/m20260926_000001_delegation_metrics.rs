use sea_orm_migration::prelude::*;

/// Queryable metrics columns for the delegation ledger (upstream #724).
///
/// Everything here is a PROJECTION of data the row already carries — the
/// terminal `DelegationTaskReport` JSON (`terminal_report`) or the admission
/// `ResumeBinding` (`resume_binding`) — extracted into plain columns so the
/// performance dashboard can filter / group without parsing JSON in every
/// row. The JSON blobs stay untouched and remain the source of truth; the
/// columns are written exactly once, in the same statement as the blob on
/// the freeze path, so they cannot drift.
///
/// Column → source:
///   * `agent_type` — `resume_binding.agent_type` (known at admission; set
///     by `admit`, so even still-running rows group correctly)
///   * `error_code`, `duration_ms`, `turn_count`, `input_tokens`,
///     `output_tokens` — the terminal report's same-named fields
///   * `effective_model` / `effective_mode` / `effective_reasoning_level` —
///     `report.selectors.effective`: `config_values["model"]`,
///     `effective.mode`, `config_values["reasoning_effort"]`. NULL when the
///     call asked for no selectors (every pre-feature row) or the child
///     never produced an admission snapshot.
///
/// Deliberately NOT extracted:
///   * rework rounds — the `source_task_id` chain already encodes them;
///     the aggregator derives rework rate at read time
///   * a "final acceptance" verdict — that is a USER judgment that does not
///     exist yet; when #731's evaluation loop lands one, it belongs in its
///     own nullable `verdict` column beside these, not in the report JSON.
///
/// Pre-existing rows keep NULL metrics columns: their terminal reports
/// predate `turn_count` / `token_usage` on the wire, and inventing zeros
/// would understate real costs. Aggregation treats NULL as "not reported".
#[derive(DeriveMigrationName)]
pub struct Migration;

const IDX_DELEGATION_TASK_AGENT_TYPE: &str = "idx_delegation_task_agent_type";
const IDX_DELEGATION_TASK_EFFECTIVE_MODEL: &str = "idx_delegation_task_effective_model";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One ALTER per column: SQLite's ADD COLUMN takes a single column,
        // matching the one-column-per-alter convention of earlier migrations.
        type Def = fn(DelegationTask) -> ColumnDef;
        let columns: [(DelegationTask, Def); 9] = [
            (DelegationTask::AgentType, |c| {
                ColumnDef::new(c).text().null().take()
            }),
            (DelegationTask::ErrorCode, |c| {
                ColumnDef::new(c).text().null().take()
            }),
            (DelegationTask::DurationMs, |c| {
                ColumnDef::new(c).big_integer().null().take()
            }),
            (DelegationTask::TurnCount, |c| {
                ColumnDef::new(c).integer().null().take()
            }),
            (DelegationTask::InputTokens, |c| {
                ColumnDef::new(c).big_integer().null().take()
            }),
            (DelegationTask::OutputTokens, |c| {
                ColumnDef::new(c).big_integer().null().take()
            }),
            (DelegationTask::EffectiveModel, |c| {
                ColumnDef::new(c).text().null().take()
            }),
            (DelegationTask::EffectiveMode, |c| {
                ColumnDef::new(c).text().null().take()
            }),
            (DelegationTask::EffectiveReasoningLevel, |c| {
                ColumnDef::new(c).text().null().take()
            }),
        ];
        for (column, def) in columns {
            manager
                .alter_table(
                    Table::alter()
                        .table(DelegationTask::Table)
                        .add_column(def(column))
                        .to_owned(),
                )
                .await?;
        }

        // The dashboard groups by both dimensions; without these the
        // aggregation is a full scan over the ledger. One row per delegation
        // keeps both indexes tiny.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(IDX_DELEGATION_TASK_AGENT_TYPE)
                    .table(DelegationTask::Table)
                    .col(DelegationTask::AgentType)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(IDX_DELEGATION_TASK_EFFECTIVE_MODEL)
                    .table(DelegationTask::Table)
                    .col(DelegationTask::EffectiveModel)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(IDX_DELEGATION_TASK_EFFECTIVE_MODEL)
                    .table(DelegationTask::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(IDX_DELEGATION_TASK_AGENT_TYPE)
                    .table(DelegationTask::Table)
                    .to_owned(),
            )
            .await?;
        for column in [
            DelegationTask::EffectiveReasoningLevel,
            DelegationTask::EffectiveMode,
            DelegationTask::EffectiveModel,
            DelegationTask::OutputTokens,
            DelegationTask::InputTokens,
            DelegationTask::TurnCount,
            DelegationTask::DurationMs,
            DelegationTask::ErrorCode,
            DelegationTask::AgentType,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(DelegationTask::Table)
                        .drop_column(column)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum DelegationTask {
    Table,
    AgentType,
    ErrorCode,
    DurationMs,
    TurnCount,
    InputTokens,
    OutputTokens,
    EffectiveModel,
    EffectiveMode,
    EffectiveReasoningLevel,
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

    /// The metrics columns are a pure additive ALTER on an existing ledger:
    /// upgrading a database that already carries delegation history must
    /// leave every legacy row byte-identical (metrics columns NULL) and
    /// still expose the new columns for post-upgrade writes.
    #[tokio::test]
    async fn adds_metrics_columns_without_touching_existing_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pre-metrics.db");
        let url = format!("sqlite:{}?mode=rwc", path.to_string_lossy());
        let conn = Database::connect(url.clone()).await.expect("open db");
        conn.execute(sql("PRAGMA foreign_keys=ON;"))
            .await
            .expect("foreign keys");

        let migrations = <Migrator as MigratorTrait>::migrations();
        let metrics_idx = migrations
            .iter()
            .position(|migration| migration.name() == "m20260926_000001_delegation_metrics")
            .expect("metrics migration is registered");
        Migrator::up(&conn, Some(metrics_idx as u32))
            .await
            .expect("apply pre-metrics schema");

        // One legacy row, inserted with raw SQL exactly as the pre-metrics
        // binary would have written it (running, no terminal report, no
        // metrics columns — they did not exist yet).
        let folder = folder_service::add_folder(&conn, "/workspace/legacy")
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
             ('legacy-task', {p}, {c}, NULL, 'legacy work', NULL, \
              'running', NULL, '{{\"agent_type\":\"codex\"}}', 0, \
              '2026-09-01 00:00:00+00:00', '2026-09-01 00:00:00+00:00')",
            p = parent.id,
            c = child.id,
        )))
        .await
        .expect("insert legacy row");

        conn.close().await.expect("close pre-metrics db");

        // Reopen so no connection-local schema cache can make the upgrade
        // easier than a real application restart.
        let conn = Database::connect(url).await.expect("reopen for upgrade");
        conn.execute(sql("PRAGMA foreign_keys=ON;"))
            .await
            .expect("foreign keys after reopen");
        Migrator::up(&conn, Some(metrics_idx as u32 + 1))
            .await
            .expect("apply metrics migration");

        for expected in [
            "agent_type",
            "error_code",
            "duration_ms",
            "turn_count",
            "input_tokens",
            "output_tokens",
            "effective_model",
            "effective_mode",
            "effective_reasoning_level",
        ] {
            let found = conn
                .query_one(sql(&format!(
                    "SELECT COUNT(*) AS n FROM pragma_table_info('delegation_task') \
                     WHERE name = '{expected}'"
                )))
                .await
                .expect("probe column")
                .expect("row")
                .try_get::<i64>("", "n")
                .expect("count");
            assert_eq!(found, 1, "column {expected} must exist after upgrade");
        }
        let after = conn
            .query_one(sql(
                "SELECT COUNT(*) AS n FROM delegation_task WHERE agent_type IS NULL \
                 AND error_code IS NULL AND duration_ms IS NULL AND turn_count IS NULL \
                 AND input_tokens IS NULL AND output_tokens IS NULL \
                 AND effective_model IS NULL AND effective_mode IS NULL \
                 AND effective_reasoning_level IS NULL",
            ))
            .await
            .expect("probe after upgrade")
            .expect("row")
            .try_get::<i64>("", "n")
            .expect("count");
        assert_eq!(after, 1, "the upgrade must not invent metrics");
        conn.close().await.expect("close upgraded db");
    }
}
