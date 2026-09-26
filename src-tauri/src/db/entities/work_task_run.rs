use sea_orm::entity::prelude::*;

/// One execution generation of a work task — the durable record behind strict
/// session continuation and the per-run cost/outcome ledger.
///
/// Rows are written by the task engine at launch (one per `run_seq`, enforced
/// by the unique index) and closed when the generation ends — including the
/// launch paths that unwind before an agent turn ever starts. The table is
/// append-only history: nothing here gates the state machine.
///
/// `resume_outcome` is the phase-2 contract that a rework round continues the
/// previous session instead of silently cold-starting: `resumed` (strict
/// continuation of `resumed_from_run_seq`'s session), `fresh_no_session` (the
/// agent never reported a session to continue — a first run, not a lost one),
/// `fresh_requested` (the user explicitly asked for a new session),
/// `fallback_cold` (best-effort resume fell back; merge generations only), or
/// `strict_failed` (the continuation was refused and the run did not start).
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "work_task_run")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub task_id: i32,
    /// The generation this run belongs to (`work_task.run_seq`).
    pub run_seq: i32,
    /// fresh | retry | return | merge
    pub kind: String,
    /// running | settled | failed | canceled
    pub status: String,
    /// resumed | fresh_no_session | fresh_requested | fallback_cold |
    /// strict_failed
    pub resume_outcome: Option<String>,
    /// The run whose session this one continued.
    pub resumed_from_run_seq: Option<i32>,
    pub agent_type: Option<String>,
    pub conversation_id: Option<i32>,
    /// Agent-assigned session identity captured for this run — the anchor a
    /// strict continuation resumes.
    pub external_session_id: Option<String>,
    pub working_dir: Option<String>,
    pub effective_mode: Option<String>,
    pub effective_model: Option<String>,
    pub effective_reasoning_level: Option<String>,
    pub started_at: DateTimeUtc,
    pub finished_at: Option<DateTimeUtc>,
    pub duration_ms: Option<i64>,
    /// Token facts folded in at close time (best-effort).
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub verdict: Option<String>,
    /// agent_error | setup_error | verdict_blocked | interrupted | resume_failed
    pub error_code: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::work_task::Entity",
        from = "Column::TaskId",
        to = "super::work_task::Column::Id"
    )]
    Task,
}

impl ActiveModelBehavior for ActiveModel {}
