use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // work_task_batch: a group of existing work tasks that share one
        // immutable starting commit and answer to aggregate commands. It owns
        // NO member lifecycle — the task engine remains the single execution
        // authority; a batch only pins the common base, caps concurrency,
        // decides what a member failure means for the rest, and projects the
        // members' statuses into one row clients can read without N queries.
        manager
            .create_table(
                Table::create()
                    .table(WorkTaskBatch::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(WorkTaskBatch::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // The project folder every member targets (never a
                    // worktree folder). Soft reference, same discipline as
                    // work_task.folder_id.
                    .col(
                        ColumnDef::new(WorkTaskBatch::FolderId)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(WorkTaskBatch::Title).text().not_null())
                    // The commit resolved ONCE at creation. Every member's
                    // worktree starts here regardless of how the project
                    // folder's HEAD moves afterwards — that is the whole point
                    // of a batch, and the reason a comparison is meaningful.
                    .col(ColumnDef::new(WorkTaskBatch::BaseSha).text().not_null())
                    // The branch HEAD pointed at when `base_sha` was resolved;
                    // recorded so a member's diff and merge have a named base.
                    .col(
                        ColumnDef::new(WorkTaskBatch::BaseBranch)
                            .text()
                            .not_null(),
                    )
                    // created | running | settled | canceled. A projection of
                    // the members, written by the aggregation pass — never a
                    // second state machine.
                    .col(
                        ColumnDef::new(WorkTaskBatch::Status)
                            .text()
                            .not_null()
                            .default("created"),
                    )
                    // best_effort | fail_fast — what a member's failure means
                    // for the members that have not started yet.
                    .col(
                        ColumnDef::new(WorkTaskBatch::FailurePolicy)
                            .text()
                            .not_null()
                            .default("best_effort"),
                    )
                    // Members launched at once; NULL = defer to the folder's
                    // own `max_concurrent`.
                    .col(ColumnDef::new(WorkTaskBatch::MaxConcurrent).integer())
                    // Reverse-DNS id of the module that created this batch
                    // ('vendor.module'), or NULL for a plain bulk operation.
                    // Opaque to Core: never branched on, only carried.
                    .col(ColumnDef::new(WorkTaskBatch::OwnerExtension).text())
                    // JSON, owner-defined. Same discipline as
                    // work_task.config: replayed and displayed, never queried.
                    .col(ColumnDef::new(WorkTaskBatch::Metadata).text())
                    .col(
                        ColumnDef::new(WorkTaskBatch::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(WorkTaskBatch::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    // Every member reached a terminal status.
                    .col(
                        ColumnDef::new(WorkTaskBatch::SettledAt)
                            .timestamp_with_time_zone(),
                    )
                    .col(
                        ColumnDef::new(WorkTaskBatch::DeletedAt)
                            .timestamp_with_time_zone(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_work_task_batch_folder")
                    .table(WorkTaskBatch::Table)
                    .col(WorkTaskBatch::FolderId)
                    .to_owned(),
            )
            .await?;

        // work_task_batch_member: the batch ↔ task edge, plus the per-member
        // facts a batch adds on top of a task. Deliberately a join table
        // rather than a `batch_id` column on `work_task`: the task table (and
        // the engine that owns it) stays untouched, and the batch-specific
        // columns — slot order, display label, launch snapshot — live where
        // they belong instead of widening every task row.
        manager
            .create_table(
                Table::create()
                    .table(WorkTaskBatchMember::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(WorkTaskBatchMember::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(WorkTaskBatchMember::BatchId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(WorkTaskBatchMember::TaskId)
                            .integer()
                            .not_null(),
                    )
                    // Stable presentation order (slot 0, 1, 2 …). The owner
                    // decides what a slot means; Core only keeps the order.
                    .col(
                        ColumnDef::new(WorkTaskBatchMember::SlotIndex)
                            .integer()
                            .not_null(),
                    )
                    // Owner-supplied display name for this member.
                    .col(ColumnDef::new(WorkTaskBatchMember::Label).text())
                    // JSON `ResolvedLaunchProfile` captured when the member was
                    // added: what configuration was requested, what actually
                    // applied, and every gap between them. This is what makes a
                    // comparison honest — a report can state the real
                    // configuration instead of the one the user hoped for.
                    .col(
                        ColumnDef::new(WorkTaskBatchMember::ProfileSnapshot)
                            .text(),
                    )
                    // Result of the last aggregate cleanup for this member:
                    // succeeded | failed | blocked. NULL = never attempted.
                    // Persisted so a failed cleanup survives the window that
                    // asked for it — the UI must never report a success the
                    // backend did not achieve.
                    .col(ColumnDef::new(WorkTaskBatchMember::CleanupResult).text())
                    .col(ColumnDef::new(WorkTaskBatchMember::CleanupError).text())
                    .col(
                        ColumnDef::new(WorkTaskBatchMember::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // A task belongs to at most one batch: two batches pinning different
        // base commits onto one task have no coherent meaning.
        manager
            .create_index(
                Index::create()
                    .name("idx_work_task_batch_member_task")
                    .table(WorkTaskBatchMember::Table)
                    .col(WorkTaskBatchMember::TaskId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_work_task_batch_member_batch")
                    .table(WorkTaskBatchMember::Table)
                    .col(WorkTaskBatchMember::BatchId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(WorkTaskBatchMember::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(WorkTaskBatch::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum WorkTaskBatch {
    Table,
    Id,
    FolderId,
    Title,
    BaseSha,
    BaseBranch,
    Status,
    FailurePolicy,
    MaxConcurrent,
    OwnerExtension,
    Metadata,
    CreatedAt,
    UpdatedAt,
    SettledAt,
    DeletedAt,
}

#[derive(DeriveIden)]
enum WorkTaskBatchMember {
    Table,
    Id,
    BatchId,
    TaskId,
    SlotIndex,
    Label,
    ProfileSnapshot,
    CleanupResult,
    CleanupError,
    CreatedAt,
}
