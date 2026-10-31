use sea_orm::entity::prelude::*;

/// Durable admission record for one ordinary delegation execution.
///
/// Conversation rows are foreign-key parents. Soft deletion still hides
/// history from lookup, while physical deletion cascades the ledger row.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_task")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub task_id: String,
    pub parent_conversation_id: i32,
    pub child_conversation_id: i32,
    pub source_task_id: Option<String>,
    pub task: String,
    pub requested_working_dir: Option<String>,
    /// Current execution status. The terminal report is immutable and lives in
    /// `terminal_report`; this column is not derived from the child row.
    pub status: String,
    /// JSON snapshot of the exact terminal `DelegationTaskReport`.
    pub terminal_report: Option<String>,
    /// JSON [`ResumeBinding`](crate::db::service::delegation_task_service::ResumeBinding).
    pub resume_binding: String,
    pub released: bool,
    /// The work task whose execution produced this delegation, when the
    /// delegating parent conversation was running a work task. Resolved once
    /// at admission and stored, so the link survives later session changes on
    /// the task (a fresh-session rework repoints `work_task.conversation_id`).
    /// `None` for ordinary chat delegations. Soft reference — `work_task`
    /// soft-deletes and the ledger keeps its history either way.
    pub work_task_id: Option<i32>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    // ── Performance dashboard projections (upstream #724) ─────────────────
    // Each column below is written by the SAME statement that freezes the
    // terminal report — never updated separately. Most values are extracted
    // from that report or the admission `resume_binding`; the token counters
    // are additionally filled from the child's materialized `token_usage_turn`
    // facts when the report carries none (it has no cache split at all). NULL
    // means "not known": pre-metrics rows, failures before admission
    // snapshots, or agents that don't report the value. The dashboard's
    // grouping keys (`agent_type`, `effective_model`) are indexed.
    /// Child agent slug (`resume_binding.agent_type`), set at admission so
    /// still-running rows group correctly.
    pub agent_type: Option<String>,
    /// Terminal `error_code` (e.g. `child_refusal`); NULL on success.
    pub error_code: Option<String>,
    /// Broker-measured wall-clock runtime of the finished task, in ms.
    pub duration_ms: Option<i64>,
    /// Child turn count of a completed task.
    pub turn_count: Option<i32>,
    /// Input tokens the child reported for the run, if any.
    pub input_tokens: Option<i64>,
    /// Output tokens the child reported for the run, if any.
    pub output_tokens: Option<i64>,
    /// Prompt-cache creation tokens; NULL when the agent/report did not say.
    pub cache_creation_tokens: Option<i64>,
    /// Prompt-cache read tokens; NULL when the agent/report did not say.
    pub cache_read_tokens: Option<i64>,
    /// Effective selectors the child launched with (from the terminal
    /// report's `selectors.effective`, or the conversation / latest usage
    /// turn when the snapshot is missing); NULL when genuinely unknown.
    pub effective_model: Option<String>,
    pub effective_mode: Option<String>,
    pub effective_reasoning_level: Option<String>,
    // Extension point (#731 evaluation loop): a user's final-acceptance
    // verdict does not exist yet. When it lands, it belongs here as its own
    // nullable `verdict` column (written by an explicit user action, not by
    // the freeze path) so the dashboard can split "completed" from
    // "completed AND accepted" without rewriting history.
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
