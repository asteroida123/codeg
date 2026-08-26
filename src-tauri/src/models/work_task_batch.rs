//! Wire types for `WorkTaskBatch` — a group of existing work tasks sharing one
//! immutable starting commit.
//!
//! Core vocabulary only. A batch has members with slots and labels; it has no
//! candidates, no evaluators, and no rounds. Whatever a caller sees in a
//! member is the caller's business, carried opaquely in `owner_extension` and
//! `metadata`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use crate::db::entities::work_task_batch::{WorkTaskBatchFailurePolicy, WorkTaskBatchStatus};
pub use crate::db::entities::work_task_batch_member::MemberCleanupResult;

/// What a create request asks for. The base commit is deliberately absent: it
/// is resolved server-side from the folder's HEAD so a client cannot pin a
/// commit the repository never had, and so every member of one batch is
/// guaranteed the same start.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkTaskBatchSpec {
    pub folder_id: i32,
    pub title: String,
    /// The members to create, in slot order. Each carries a normal task draft
    /// config — the same shape a hand-written task uses.
    pub members: Vec<WorkTaskBatchMemberSpec>,
    #[serde(default)]
    pub failure_policy: Option<WorkTaskBatchFailurePolicy>,
    /// `None` = defer to the folder's own `max_concurrent`.
    #[serde(default)]
    pub max_concurrent: Option<i32>,
    /// Reverse-DNS id of the calling module (`vendor.module`). Opaque to Core.
    #[serde(default)]
    pub owner_extension: Option<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    /// Create even though the project folder has uncommitted tracked changes.
    ///
    /// Default `false`, and that default is load-bearing: members branch from
    /// the recorded commit, so uncommitted work is silently absent from every
    /// member's starting tree. Refusing by default makes the user decide
    /// knowingly instead of discovering it in a diff. Under no value of this
    /// flag does anything commit on the user's behalf.
    #[serde(default)]
    pub allow_dirty: bool,
}

/// One member of a create request.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkTaskBatchMemberSpec {
    pub title: String,
    /// Opaque `WorkTaskConfig` — identical to what a standalone task stores.
    pub config: serde_json::Value,
    #[serde(default)]
    pub label: Option<String>,
    /// JSON `ResolvedLaunchProfile` captured by the caller before creating.
    #[serde(default)]
    pub profile_snapshot: Option<serde_json::Value>,
}

/// A batch as clients read it: the row, plus its members with their live task
/// status folded in.
#[derive(Debug, Clone, Serialize)]
pub struct WorkTaskBatchInfo {
    pub id: i32,
    pub folder_id: i32,
    pub title: String,
    pub base_sha: String,
    pub base_branch: String,
    pub status: WorkTaskBatchStatus,
    pub failure_policy: WorkTaskBatchFailurePolicy,
    pub max_concurrent: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_extension: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    pub members: Vec<WorkTaskBatchMemberInfo>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub settled_at: Option<DateTime<Utc>>,
}

/// A member as clients read it. The task's own status is read live from
/// `work_task` on every fetch rather than mirrored into the member row, so the
/// two can never disagree.
#[derive(Debug, Clone, Serialize)]
pub struct WorkTaskBatchMemberInfo {
    pub id: i32,
    pub batch_id: i32,
    pub task_id: i32,
    pub slot_index: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_snapshot: Option<serde_json::Value>,
    /// `None` = cleanup never attempted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_result: Option<MemberCleanupResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_error: Option<String>,
    /// Live status of the underlying task; `None` if the task row was deleted.
    pub task_status: Option<crate::models::WorkTaskStatus>,
    pub task_title: String,
    /// Live conversation of the member's current generation — what a transcript
    /// view attaches to.
    pub conversation_id: Option<i32>,
    pub connection_id: Option<String>,
    pub files_changed: Option<i32>,
    pub additions: Option<i32>,
    pub deletions: Option<i32>,
}

/// Per-member outcome of an aggregate command. Every aggregate returns one of
/// these **per member** rather than a single boolean: the review this type
/// answers to found a UI that reported one success while every member's cleanup
/// had in fact been refused.
#[derive(Debug, Clone, Serialize)]
pub struct BatchMemberOutcome {
    pub task_id: i32,
    pub slot_index: i32,
    /// `true` = the backend did what was asked for this member.
    pub ok: bool,
    /// Why not, verbatim from the backend, when `ok` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What an aggregate cleanup did, per member.
#[derive(Debug, Clone, Serialize)]
pub struct BatchCleanupOutcome {
    pub task_id: i32,
    pub slot_index: i32,
    pub result: MemberCleanupResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Which members an aggregate cleanup should touch.
#[derive(Debug, Clone, Deserialize)]
pub struct BatchCleanupPolicy {
    /// Task ids to keep. Everything else in the batch is cleaned. Empty = clean
    /// every member.
    #[serde(default)]
    pub keep_task_ids: Vec<i32>,
}
