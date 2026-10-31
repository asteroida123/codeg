use sea_orm_migration::prelude::*;

/// Split the delegation ledger's token cost into the cache counters the token
/// dashboard already reports.
///
/// `input_tokens` / `output_tokens` alone understate agents whose prompts are
/// mostly served from (or written to) the prompt cache — Claude Code and
/// CodeBuddy are the common cases. These two columns mirror
/// `token_usage_turn.cache_creation_tokens` / `cache_read_tokens`, filled from
/// the child's turn facts on the same freeze write as every other metric.
///
/// Both are nullable and NULL means "not known", exactly like the existing
/// token columns: pre-migration rows and agents that report no usage must not
/// be turned into zeros.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [
            DelegationTask::CacheCreationTokens,
            DelegationTask::CacheReadTokens,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(DelegationTask::Table)
                        .add_column(ColumnDef::new(column).big_integer().null().take())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [
            DelegationTask::CacheReadTokens,
            DelegationTask::CacheCreationTokens,
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
    CacheCreationTokens,
    CacheReadTokens,
}
