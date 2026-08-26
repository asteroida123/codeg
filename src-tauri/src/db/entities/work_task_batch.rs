use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// Aggregate state of a batch. A **projection of its members**, not a second
/// state machine: the only writer is the aggregation pass that runs when a
/// member's status changes, and every value here is recomputable from the
/// member rows. It exists so a client reads one row instead of N, and so
/// "this batch is finished" survives a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum WorkTaskBatchStatus {
    /// Members exist but none was ever launched.
    #[sea_orm(string_value = "created")]
    Created,
    /// At least one member is live (queued → merging).
    #[sea_orm(string_value = "running")]
    Running,
    /// No member is working any more and at least one is waiting for the user
    /// to accept, return, or drop it.
    ///
    /// A state of its own because this is the moment a batch exists for: every
    /// agent has stopped, the results are all in, and the comparison is ready to
    /// read. Folding it into `created` (nothing is live, not everything is
    /// terminal) would describe a finished round as one that never started, and
    /// folding it into `settled` would claim members are finished while each
    /// still holds a worktree and an open decision.
    #[sea_orm(string_value = "review")]
    Review,
    /// Every member reached a terminal status on its own.
    #[sea_orm(string_value = "settled")]
    Settled,
    /// The user canceled the whole batch. Distinct from `settled` so the UI can
    /// explain why members stopped, and so a late member event cannot present a
    /// canceled batch as a completed one.
    #[sea_orm(string_value = "canceled")]
    Canceled,
}

/// What a member's failure means for the members that have not launched yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum WorkTaskBatchFailurePolicy {
    /// Keep going; a comparison of three agents is still useful when one fails.
    #[sea_orm(string_value = "best_effort")]
    BestEffort,
    /// Cancel the rest on the first failure.
    #[sea_orm(string_value = "fail_fast")]
    FailFast,
}

/// A group of existing work tasks sharing one immutable starting commit.
///
/// What a batch owns: the common base (`base_sha` / `base_branch`), the launch
/// order and concurrency cap, the failure policy, and the aggregate projection
/// above. What it explicitly does NOT own: member status, worktrees,
/// connections, cancellation, or cleanup — all of that stays with the task
/// engine, which remains the single execution authority. A batch never writes
/// a `work_task` row.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "work_task_batch")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// The project folder every member targets. Soft reference; queries join
    /// the live folder so a removed project hides its batches.
    pub folder_id: i32,
    pub title: String,
    /// Resolved ONCE at creation, from the project folder's HEAD. Members start
    /// here however the folder's HEAD moves afterwards — a shared start is what
    /// makes comparing their results meaningful.
    pub base_sha: String,
    /// The branch HEAD pointed at when `base_sha` was resolved.
    pub base_branch: String,
    pub status: WorkTaskBatchStatus,
    pub failure_policy: WorkTaskBatchFailurePolicy,
    /// Members launched at once; `None` = defer to the folder's own
    /// `max_concurrent`.
    pub max_concurrent: Option<i32>,
    /// Reverse-DNS id of the module that created the batch (`vendor.module`), or
    /// `None` for a plain bulk operation. Opaque to Core — carried and
    /// returned, never branched on. This is the seam that keeps business nouns
    /// out of the engine.
    pub owner_extension: Option<String>,
    /// JSON, owner-defined. Replayed and displayed, never queried.
    #[sea_orm(column_type = "Text")]
    pub metadata: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    /// Every member reached a terminal status.
    pub settled_at: Option<DateTimeUtc>,
    pub deleted_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::work_task_batch_member::Entity")]
    Members,
    #[sea_orm(
        belongs_to = "super::folder::Entity",
        from = "Column::FolderId",
        to = "super::folder::Column::Id"
    )]
    Folder,
}

impl Related<super::work_task_batch_member::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Members.def()
    }
}

impl Related<super::folder::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Folder.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
