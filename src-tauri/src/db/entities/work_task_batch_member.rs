use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// Outcome of the last aggregate cleanup attempt on a member.
///
/// Persisted rather than returned-and-forgotten, and that is the point: the
/// review this schema answers to found a UI reporting cleanup success while N
/// full repository copies stayed on disk forever, because the failures were
/// swallowed by a `Promise.allSettled` and the paths cleared anyway. A failure
/// that survives in the row can be shown, retried, and audited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum MemberCleanupResult {
    /// The worktree and branch are gone.
    #[sea_orm(string_value = "succeeded")]
    Succeeded,
    /// The removal was attempted and refused. Retryable; the tree is intact.
    #[sea_orm(string_value = "failed")]
    Failed,
    /// Not attempted because the member is still live. Not an error — cancel or
    /// finish it first.
    #[sea_orm(string_value = "blocked")]
    Blocked,
}

/// The batch ↔ task edge plus the per-member facts a batch adds on top of a
/// task.
///
/// A join table on purpose: `work_task` and the 10k-line engine that owns it
/// stay untouched, and the batch-specific columns live here instead of widening
/// every task row in the database. The `task_id` unique index enforces that a
/// task belongs to at most one batch — two batches pinning different base
/// commits onto one task have no coherent meaning.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "work_task_batch_member")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub batch_id: i32,
    /// The existing work task this member is. Its status, worktree, and
    /// conversation are read from `work_task` — never mirrored here, so the two
    /// can never disagree.
    pub task_id: i32,
    /// Stable presentation order. The owner decides what a slot means (a
    /// compared candidate, a pipeline role, a plain position); Core only keeps
    /// the order.
    pub slot_index: i32,
    pub label: Option<String>,
    /// Caller-defined JSON snapshot of what it REQUESTED for this member at
    /// creation (the Arena stores `{ requested: { agent_type, mode_id,
    /// config_values } }`). Not the engine's `ResolvedLaunchProfile`: what
    /// actually applied is derived at read time from the task's
    /// `config_effective` events, which only exist once a process has launched.
    #[sea_orm(column_type = "Text")]
    pub profile_snapshot: Option<String>,
    /// `None` = cleanup never attempted for this member.
    pub cleanup_result: Option<MemberCleanupResult>,
    /// The backend's own words for a `failed` cleanup, kept for the retry.
    pub cleanup_error: Option<String>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::work_task_batch::Entity",
        from = "Column::BatchId",
        to = "super::work_task_batch::Column::Id"
    )]
    Batch,
    #[sea_orm(
        belongs_to = "super::work_task::Entity",
        from = "Column::TaskId",
        to = "super::work_task::Column::Id"
    )]
    Task,
}

impl Related<super::work_task_batch::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Batch.def()
    }
}

impl Related<super::work_task::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Task.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
