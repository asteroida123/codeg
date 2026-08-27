//! `WorkTaskBatch` persistence: create, read, and the aggregate projection.
//!
//! Mode-agnostic like every other service here — plain `&DatabaseConnection`,
//! so Tauri commands, Axum handlers, and the task engine share one code path.
//!
//! What this module deliberately does NOT do:
//! - It never writes a `work_task` status. Members are launched, canceled, and
//!   cleaned by the task engine; a batch only asks. Two writers on one status
//!   column is exactly the split-authority bug this design exists to avoid.
//! - It never runs a state machine of its own. [`recompute_status`] derives the
//!   batch's status from its members and stores the result; the members remain
//!   the only truth. A stored projection buys one query instead of N and
//!   survives a restart — it buys no authority.
//! - It never commits, stashes, or otherwise touches the user's working tree.
//!   The base commit is *read*; a dirty repository is refused, not tidied.

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, ConnectionTrait, DatabaseConnection,
    EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};

use crate::db::entities::work_task::WorkTaskStatus;
use crate::db::entities::work_task_batch::{WorkTaskBatchFailurePolicy, WorkTaskBatchStatus};
use crate::db::entities::work_task_batch_member::MemberCleanupResult;
use crate::db::entities::{folder, work_task, work_task_batch, work_task_batch_member};
use crate::db::error::DbError;
use crate::models::{
    WorkTaskBatchInfo, WorkTaskBatchMemberInfo, WorkTaskBatchMemberSpec, WorkTaskBatchSpec,
};

/// Upper bound on members of one batch. Not a product opinion — a guard: each
/// member is a full worktree (a complete checkout of the repository) plus an
/// agent process, so an unbounded count is a disk-and-memory hazard rather than
/// a feature. Callers that want fewer slots enforce their own limit.
pub const MAX_MEMBERS: usize = 16;

/// Terminal statuses: a member in one of these will not change on its own.
pub fn is_terminal(status: WorkTaskStatus) -> bool {
    matches!(
        status,
        WorkTaskStatus::Done | WorkTaskStatus::Failed | WorkTaskStatus::Canceled
    )
}

/// Live statuses: the member holds — or is about to hold — an execution slot.
pub fn is_live(status: WorkTaskStatus) -> bool {
    matches!(
        status,
        WorkTaskStatus::Queued
            | WorkTaskStatus::Preparing
            | WorkTaskStatus::Running
            | WorkTaskStatus::AwaitingInput
            | WorkTaskStatus::Merging
    )
}

// ── create ─────────────────────────────────────────────────────────────────

/// Validated pieces of a create request, resolved by the caller.
///
/// `base_sha` / `base_branch` arrive already read from git by the command
/// layer: this service never shells out. They are recorded verbatim, and every
/// member's worktree is later pinned to them by the engine.
#[derive(Debug, Clone)]
pub struct ResolvedBase {
    pub sha: String,
    pub branch: String,
}

/// Create a batch and its member tasks in ONE transaction.
///
/// All-or-nothing on purpose: a half-created batch would leave orphan tasks the
/// user never asked for, on a base no longer recorded anywhere. The member tasks
/// are ordinary `work_task` rows in `todo` — nothing is launched here, exactly
/// like `work_task_service::create`.
pub async fn create(
    conn: &DatabaseConnection,
    spec: &WorkTaskBatchSpec,
    base: ResolvedBase,
) -> Result<WorkTaskBatchInfo, DbError> {
    validate_spec(spec)?;

    // Same folder discipline as a standalone task: a live project root, never a
    // worktree folder (a batch member rooted in a worktree would nest worktrees
    // at launch).
    let folder = folder::Entity::find_by_id(spec.folder_id)
        .one(conn)
        .await?
        .filter(|f| f.deleted_at.is_none())
        .ok_or_else(|| DbError::NotFound(format!("folder {}", spec.folder_id)))?;
    if folder.parent_id.is_some() {
        return Err(DbError::Validation(
            "a batch must target a project folder, not a worktree".into(),
        ));
    }

    let now = Utc::now();
    let metadata = match &spec.metadata {
        Some(v) => Some(
            serde_json::to_string(v)
                .map_err(|e| DbError::Validation(format!("metadata not serializable: {e}")))?,
        ),
        None => None,
    };

    // Member configs are serialized BEFORE the transaction opens: a malformed
    // config should fail the request outright, not roll a write back.
    let mut member_configs = Vec::with_capacity(spec.members.len());
    for m in &spec.members {
        member_configs.push((
            m,
            serde_json::to_string(&m.config)
                .map_err(|e| DbError::Validation(format!("member config not serializable: {e}")))?,
            match &m.profile_snapshot {
                Some(p) => Some(serde_json::to_string(p).map_err(|e| {
                    DbError::Validation(format!("profile snapshot not serializable: {e}"))
                })?),
                None => None,
            },
        ));
    }

    let max_order = work_task::Entity::find()
        .filter(work_task::Column::FolderId.eq(spec.folder_id))
        .order_by_desc(work_task::Column::SortOrder)
        .one(conn)
        .await?
        .map(|m| m.sort_order)
        .unwrap_or(0);

    let txn = conn.begin().await?;

    let batch = work_task_batch::ActiveModel {
        id: NotSet,
        folder_id: Set(spec.folder_id),
        title: Set(spec.title.trim().to_string()),
        base_sha: Set(base.sha.clone()),
        base_branch: Set(base.branch.clone()),
        status: Set(WorkTaskBatchStatus::Created),
        failure_policy: Set(spec
            .failure_policy
            .unwrap_or(WorkTaskBatchFailurePolicy::BestEffort)),
        max_concurrent: Set(spec.max_concurrent.filter(|n| *n > 0)),
        owner_extension: Set(spec.owner_extension.clone()),
        metadata: Set(metadata),
        created_at: Set(now),
        updated_at: Set(now),
        settled_at: Set(None),
        deleted_at: Set(None),
    }
    .insert(&txn)
    .await?;

    for (slot, (member, config_str, profile_str)) in member_configs.into_iter().enumerate() {
        let task = work_task::ActiveModel {
            id: NotSet,
            folder_id: Set(spec.folder_id),
            title: Set(member.title.trim().to_string()),
            config: Set(config_str),
            status: Set(WorkTaskStatus::Todo),
            failure_reason: Set(None),
            last_error: Set(None),
            run_seq: Set(0),
            sort_order: Set(max_order + 1 + slot as i32),
            worktree_folder_id: Set(None),
            conversation_id: Set(None),
            connection_id: Set(None),
            base_branch: Set(None),
            base_sha: Set(None),
            work_branch: Set(None),
            merge_state: Set(None),
            pending_merge: Set(None),
            cleanup_state: Set(None),
            verdict: Set(None),
            result_summary: Set(None),
            files_changed: Set(None),
            additions: Set(None),
            deletions: Set(None),
            merge_commit: Set(None),
            completion_kind: Set(None),
            preflight: Set(None),
            archived_at: Set(None),
            scheduled_at: Set(None),
            source_kind: Set(None),
            source_key: Set(None),
            source_meta: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            started_at: Set(None),
            settled_at: Set(None),
            finished_at: Set(None),
            deleted_at: Set(None),
        }
        .insert(&txn)
        .await?;

        work_task_batch_member::ActiveModel {
            id: NotSet,
            batch_id: Set(batch.id),
            task_id: Set(task.id),
            slot_index: Set(slot as i32),
            label: Set(member.label.clone()),
            profile_snapshot: Set(profile_str),
            cleanup_result: Set(None),
            cleanup_error: Set(None),
            created_at: Set(now),
        }
        .insert(&txn)
        .await?;

        crate::db::service::work_task_service::record_event(
            &txn,
            task.id,
            "created",
            "user",
            Some(serde_json::json!({
                "batch_id": batch.id,
                "slot_index": slot,
                // The pinned base travels onto the member's own timeline: a task
                // whose start was decided elsewhere should say so where it is
                // read, not only where it was decided.
                "base_sha": base.sha,
                "base_branch": base.branch,
            })),
        )
        .await?;
    }

    txn.commit().await?;
    get(conn, batch.id).await
}

/// Group tasks that ALREADY EXIST into a batch over one pinned base.
///
/// The counterpart to [`create`], and the reason the two are separate functions
/// rather than one with a flag: `create` mints its members, `adopt` takes rows
/// the user already wrote. Nothing is created here and nothing is launched — the
/// tasks keep their own titles, configs and sort order, and gain a shared
/// starting commit plus the aggregate commands.
///
/// This is what a batch looks like without an app on top: pick several to-dos on
/// the board, run them from one commit, cancel them together, and clean them up
/// with a per-member answer for each. Every one of those is something the board
/// could not do task-by-task — in particular there was no way to remove several
/// worktrees and be told individually which removals actually succeeded.
///
/// Four refusals, each because the alternative would silently mean something
/// other than what the user asked for:
///
/// - **A task that already has a worktree.** Its starting commit was fixed when
///   that worktree was created, so a batch "pinning" it would record a base it
///   does not have. The board's own requeue keeps the worktree deliberately;
///   this refuses instead of quietly disagreeing with it.
/// - **A task that is not `todo`.** A running or reviewed task's start is behind
///   it. Grouping it would produce a batch whose shared base is fiction for that
///   member.
/// - **A task from another folder.** A batch's base is one repository's commit.
/// - **A task already in a batch.** Two batches pinning different commits onto
///   one task has no coherent meaning (the unique index enforces it; this reports
///   it as itself rather than as a constraint violation).
pub async fn adopt(
    conn: &DatabaseConnection,
    folder_id: i32,
    title: &str,
    task_ids: &[i32],
    base: ResolvedBase,
) -> Result<WorkTaskBatchInfo, DbError> {
    if title.trim().is_empty() {
        return Err(DbError::Validation("batch title is required".into()));
    }
    if task_ids.is_empty() {
        return Err(DbError::Validation(
            "select at least one task to run as a batch".into(),
        ));
    }
    if task_ids.len() > MAX_MEMBERS {
        return Err(DbError::Validation(format!(
            "a batch takes at most {MAX_MEMBERS} members, got {}",
            task_ids.len()
        )));
    }
    // A repeated id would take two slots for one task and then trip the unique
    // index mid-transaction; caught here so the message names the real problem.
    let mut seen = std::collections::BTreeSet::new();
    for id in task_ids {
        if !seen.insert(*id) {
            return Err(DbError::Validation(format!(
                "task {id} was selected more than once"
            )));
        }
    }

    let folder = folder::Entity::find_by_id(folder_id)
        .one(conn)
        .await?
        .filter(|f| f.deleted_at.is_none())
        .ok_or_else(|| DbError::NotFound(format!("folder {folder_id}")))?;
    if folder.parent_id.is_some() {
        return Err(DbError::Validation(
            "a batch must target a project folder, not a worktree".into(),
        ));
    }

    // Every eligibility check runs BEFORE the first write, so a rejected
    // selection never leaves a batch row behind — and the user gets one clear
    // reason instead of a partially-formed group.
    for id in task_ids {
        let task = work_task::Entity::find_by_id(*id)
            .one(conn)
            .await?
            .filter(|t| t.deleted_at.is_none())
            .ok_or_else(|| DbError::NotFound(format!("task {id}")))?;
        if task.folder_id != folder_id {
            return Err(DbError::Validation(format!(
                "task {id} belongs to another project — a batch shares one repository's commit"
            )));
        }
        if task.status != WorkTaskStatus::Todo {
            return Err(DbError::Validation(format!(
                "task {id} is {} — only to-do tasks can join a batch, because a batch fixes \
                 where its members start",
                crate::db::service::work_task_service::status_str(task.status)
            )));
        }
        if task.worktree_folder_id.is_some() {
            return Err(DbError::Validation(format!(
                "task {id} already has a worktree, so its starting commit is already decided — \
                 remove the worktree first, or leave it out of the batch"
            )));
        }
        if batch_of_task(conn, *id).await?.is_some() {
            return Err(DbError::Validation(format!(
                "task {id} already belongs to a batch"
            )));
        }
    }

    let now = Utc::now();
    let txn = conn.begin().await?;

    let batch = work_task_batch::ActiveModel {
        id: NotSet,
        folder_id: Set(folder_id),
        title: Set(title.trim().to_string()),
        base_sha: Set(base.sha.clone()),
        base_branch: Set(base.branch.clone()),
        status: Set(WorkTaskBatchStatus::Created),
        failure_policy: Set(WorkTaskBatchFailurePolicy::BestEffort),
        max_concurrent: Set(None),
        // No owner: a plain bulk operation, not an app's round. Which is exactly
        // what makes this the primitive's second consumer.
        owner_extension: Set(None),
        metadata: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        settled_at: Set(None),
        deleted_at: Set(None),
    }
    .insert(&txn)
    .await?;

    for (slot, task_id) in task_ids.iter().enumerate() {
        work_task_batch_member::ActiveModel {
            id: NotSet,
            batch_id: Set(batch.id),
            task_id: Set(*task_id),
            slot_index: Set(slot as i32),
            // No label: the task's own title is the name here. An app supplies a
            // label because "slot 2" means something to it; a bulk selection has
            // nothing to add to what the user already called the task.
            label: Set(None),
            profile_snapshot: Set(None),
            cleanup_result: Set(None),
            cleanup_error: Set(None),
            created_at: Set(now),
        }
        .insert(&txn)
        .await?;

        crate::db::service::work_task_service::record_event(
            &txn,
            *task_id,
            "batch_joined",
            "user",
            Some(serde_json::json!({
                "batch_id": batch.id,
                "slot_index": slot,
                "base_sha": base.sha,
                "base_branch": base.branch,
            })),
        )
        .await?;
    }

    txn.commit().await?;
    get(conn, batch.id).await
}

fn validate_spec(spec: &WorkTaskBatchSpec) -> Result<(), DbError> {
    if spec.title.trim().is_empty() {
        return Err(DbError::Validation("batch title is required".into()));
    }
    if spec.members.is_empty() {
        return Err(DbError::Validation("a batch needs at least one member".into()));
    }
    if spec.members.len() > MAX_MEMBERS {
        return Err(DbError::Validation(format!(
            "a batch takes at most {MAX_MEMBERS} members, got {}",
            spec.members.len()
        )));
    }
    for (i, m) in spec.members.iter().enumerate() {
        if m.title.trim().is_empty() {
            return Err(DbError::Validation(format!("member {i} has no title")));
        }
    }
    Ok(())
}

// ── queries ────────────────────────────────────────────────────────────────

/// One batch with its members, each member's live task status folded in.
pub async fn get(conn: &DatabaseConnection, id: i32) -> Result<WorkTaskBatchInfo, DbError> {
    let batch = get_model(conn, id).await?;
    let members = load_members(conn, id).await?;
    Ok(to_info(batch, members))
}

pub async fn get_model(
    conn: &DatabaseConnection,
    id: i32,
) -> Result<work_task_batch::Model, DbError> {
    work_task_batch::Entity::find_by_id(id)
        .one(conn)
        .await?
        .filter(|b| b.deleted_at.is_none())
        .ok_or_else(|| DbError::NotFound(format!("batch {id}")))
}

/// Live batches, newest first. Joined on the live folder so a removed project
/// hides its batches, matching how tasks behave.
pub async fn list(
    conn: &DatabaseConnection,
    folder_id: Option<i32>,
) -> Result<Vec<WorkTaskBatchInfo>, DbError> {
    let mut q = work_task_batch::Entity::find()
        .filter(work_task_batch::Column::DeletedAt.is_null())
        .inner_join(folder::Entity)
        .filter(folder::Column::DeletedAt.is_null());
    if let Some(fid) = folder_id {
        q = q.filter(work_task_batch::Column::FolderId.eq(fid));
    }
    let rows = q
        .order_by_desc(work_task_batch::Column::Id)
        .all(conn)
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let members = load_members(conn, row.id).await?;
        out.push(to_info(row, members));
    }
    Ok(out)
}

/// The batch a task belongs to, if any. The engine's hook into this module:
/// cheap (unique index on `task_id`) and called on every member settle.
pub async fn batch_of_task<C: ConnectionTrait>(
    conn: &C,
    task_id: i32,
) -> Result<Option<i32>, DbError> {
    Ok(work_task_batch_member::Entity::find()
        .filter(work_task_batch_member::Column::TaskId.eq(task_id))
        .one(conn)
        .await?
        .map(|m| m.batch_id))
}

/// The pinned base of the batch a task belongs to, as `(branch, sha)`.
///
/// This is what makes a batch a batch: the engine calls it while creating a
/// member's worktree and starts there instead of at the folder's current HEAD,
/// so a branch switch between two members' launches cannot drift their starting
/// points apart.
pub async fn pinned_base_of_task(
    conn: &DatabaseConnection,
    task_id: i32,
) -> Result<Option<(String, String)>, DbError> {
    let Some(member) = work_task_batch_member::Entity::find()
        .filter(work_task_batch_member::Column::TaskId.eq(task_id))
        .one(conn)
        .await?
    else {
        return Ok(None);
    };
    let Some(batch) = work_task_batch::Entity::find_by_id(member.batch_id)
        .one(conn)
        .await?
        .filter(|b| b.deleted_at.is_none())
    else {
        return Ok(None);
    };
    Ok(Some((batch.base_branch, batch.base_sha)))
}

/// Member rows of a batch in slot order.
pub async fn member_models(
    conn: &DatabaseConnection,
    batch_id: i32,
) -> Result<Vec<work_task_batch_member::Model>, DbError> {
    Ok(work_task_batch_member::Entity::find()
        .filter(work_task_batch_member::Column::BatchId.eq(batch_id))
        .order_by_asc(work_task_batch_member::Column::SlotIndex)
        .all(conn)
        .await?)
}

async fn load_members(
    conn: &DatabaseConnection,
    batch_id: i32,
) -> Result<Vec<WorkTaskBatchMemberInfo>, DbError> {
    let members = member_models(conn, batch_id).await?;
    let mut out = Vec::with_capacity(members.len());
    for m in members {
        // Read the task live rather than mirroring its status into the member
        // row: one truth cannot disagree with itself.
        let task = work_task::Entity::find_by_id(m.task_id).one(conn).await?;
        // What the ENGINE actually resolved at launch, from the task's own audit
        // trail. The member row cannot hold this: it is written at creation, and
        // the applied values do not exist until an agent process does.
        let applied_profile = latest_applied_profile(conn, m.task_id).await?;
        out.push(WorkTaskBatchMemberInfo {
            id: m.id,
            batch_id: m.batch_id,
            task_id: m.task_id,
            slot_index: m.slot_index,
            label: m.label,
            profile_snapshot: m
                .profile_snapshot
                .as_deref()
                .and_then(|p| serde_json::from_str(p).ok()),
            applied_profile,
            preflight: task
                .as_ref()
                .and_then(|t| t.preflight.as_deref())
                .and_then(|p| serde_json::from_str(p).ok()),
            cleanup_result: m.cleanup_result,
            cleanup_error: m.cleanup_error,
            task_status: task.as_ref().map(|t| t.status),
            task_title: task
                .as_ref()
                .map(|t| t.title.clone())
                .unwrap_or_else(|| format!("task {}", m.task_id)),
            conversation_id: task.as_ref().and_then(|t| t.conversation_id),
            connection_id: task.as_ref().and_then(|t| t.connection_id.clone()),
            files_changed: task.as_ref().and_then(|t| t.files_changed),
            additions: task.as_ref().and_then(|t| t.additions),
            deletions: task.as_ref().and_then(|t| t.deletions),
        });
    }
    Ok(out)
}

/// The `profile` field of a task's most recent `config_effective` event.
///
/// That event is written by the engine on every launch (see
/// `work_task::engine`), so the newest one describes the generation the member is
/// on now — a retry with a different model must not keep reporting the old one.
/// Absent field, undecodable payload, or no event at all all read as "not
/// launched yet" rather than as an error: this is evidence for a comparison, and
/// a batch view must render without it.
async fn latest_applied_profile(
    conn: &DatabaseConnection,
    task_id: i32,
) -> Result<Option<serde_json::Value>, DbError> {
    let events = crate::db::service::work_task_service::recent_events_of_kinds(
        conn,
        task_id,
        &["config_effective"],
        1,
    )
    .await?;
    Ok(events
        .into_iter()
        .next()
        .and_then(|e| e.payload)
        .and_then(|p| p.get("profile").cloned())
        .filter(|p| !p.is_null()))
}

fn to_info(
    b: work_task_batch::Model,
    members: Vec<WorkTaskBatchMemberInfo>,
) -> WorkTaskBatchInfo {
    WorkTaskBatchInfo {
        id: b.id,
        folder_id: b.folder_id,
        title: b.title,
        base_sha: b.base_sha,
        base_branch: b.base_branch,
        status: b.status,
        failure_policy: b.failure_policy,
        max_concurrent: b.max_concurrent,
        owner_extension: b.owner_extension,
        metadata: b
            .metadata
            .as_deref()
            .and_then(|m| serde_json::from_str(m).ok()),
        members,
        created_at: b.created_at,
        updated_at: b.updated_at,
        settled_at: b.settled_at,
    }
}

// ── projection ─────────────────────────────────────────────────────────────

/// Recompute a batch's status from its members and store the result.
///
/// The rules, in order:
/// - A `canceled` batch stays canceled — checked on the read AND enforced as a
///   condition on the write, because the two are different moments and a cancel
///   can land between them. The same defence `run_seq` gives a task, one level
///   up.
/// - Any live member ⇒ `running`.
/// - Nothing live, any member in `review` ⇒ `review` (the results are in and a
///   decision is open).
/// - Every member terminal ⇒ `settled`, stamping `settled_at`.
/// - Otherwise (members still `todo`, or a settled batch reopened by a retry)
///   ⇒ `created`, clearing a stale `settled_at`.
///
/// Returns the status actually on the row afterwards — which is not always the
/// one computed here, precisely because of the cancel condition.
pub async fn recompute_status(
    conn: &DatabaseConnection,
    batch_id: i32,
) -> Result<WorkTaskBatchStatus, DbError> {
    let batch = get_model(conn, batch_id).await?;
    if batch.status == WorkTaskBatchStatus::Canceled {
        return Ok(WorkTaskBatchStatus::Canceled);
    }

    let members = member_models(conn, batch_id).await?;
    if members.is_empty() {
        return Ok(batch.status);
    }

    let mut any_live = false;
    let mut any_review = false;
    let mut all_terminal = true;
    for m in &members {
        // A member whose task row is gone is skipped rather than counted as
        // outstanding: it will never report again, so treating it as unfinished
        // would strand the batch in `running` for good.
        if let Some(task) = work_task::Entity::find_by_id(m.task_id).one(conn).await? {
            if is_live(task.status) {
                any_live = true;
            }
            if task.status == WorkTaskStatus::Review {
                any_review = true;
            }
            if !is_terminal(task.status) {
                all_terminal = false;
            }
        }
    }

    let next = if any_live {
        WorkTaskBatchStatus::Running
    } else if any_review {
        WorkTaskBatchStatus::Review
    } else if all_terminal {
        WorkTaskBatchStatus::Settled
    } else {
        WorkTaskBatchStatus::Created
    };

    let settled_at_changes = (next == WorkTaskBatchStatus::Settled) != batch.settled_at.is_some();
    if next == batch.status && !settled_at_changes {
        return Ok(next);
    }

    let now = Utc::now();
    // CAS, not a plain update — and the guard is the same one the early return
    // above makes, repeated at write time because the two are not the same
    // moment.
    //
    // The interleaving this closes: a member settles, this function reads a
    // `running` batch and computes `settled`; the user cancels the batch; then
    // this function writes. Without the condition the write lands last and the
    // cancellation the user asked for is gone from the row — the batch reports a
    // normal completion, which is exactly the class of defect the batch-level
    // cancel gate exists to prevent. The read-side check cannot cover it: it ran
    // before the cancel.
    //
    // Losing the CAS is not an error. It means the row now says something this
    // pass has no authority to change, so the pass reports what is actually
    // there.
    let updated = work_task_batch::Entity::update_many()
        .col_expr(
            work_task_batch::Column::Status,
            sea_orm::sea_query::Expr::value(next),
        )
        .col_expr(
            work_task_batch::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .col_expr(
            work_task_batch::Column::SettledAt,
            sea_orm::sea_query::Expr::value(
                (next == WorkTaskBatchStatus::Settled).then_some(now),
            ),
        )
        .filter(work_task_batch::Column::Id.eq(batch_id))
        .filter(work_task_batch::Column::Status.ne(WorkTaskBatchStatus::Canceled))
        .exec(conn)
        .await?;

    if updated.rows_affected == 0 {
        // Someone canceled between the read and the write. Report the truth on
        // the row rather than the projection this pass computed.
        return Ok(get_model(conn, batch_id)
            .await
            .map(|b| b.status)
            .unwrap_or(WorkTaskBatchStatus::Canceled));
    }
    Ok(next)
}

/// Mark a batch canceled. Idempotent, and **terminal**: nothing moves a batch
/// out of `canceled`.
///
/// One-way by choice. A cancellation is a decision the user made about the whole
/// round, and no later machine event — a member's late settle, a member the user
/// retries afterwards — has the standing to withdraw it. Retrying a member of a
/// canceled batch is allowed and behaves normally; the member is an ordinary task
/// and runs. The batch stays canceled, because that is what happened to it. A
/// caller that wants a fresh round creates a fresh batch, where the base commit
/// is resolved again — which is also the honest thing to do, since the old pin
/// may be far behind by then.
pub async fn mark_canceled(conn: &DatabaseConnection, batch_id: i32) -> Result<(), DbError> {
    let batch = get_model(conn, batch_id).await?;
    if batch.status == WorkTaskBatchStatus::Canceled {
        return Ok(());
    }
    let mut active: work_task_batch::ActiveModel = batch.into();
    active.status = Set(WorkTaskBatchStatus::Canceled);
    active.updated_at = Set(Utc::now());
    active.update(conn).await?;
    Ok(())
}

/// Record what an aggregate cleanup achieved for one member.
///
/// Every attempt writes — including the failures. That is the entire reason this
/// column exists: the review this design answers to found a UI reporting
/// cleanup success while every worktree stayed on disk, because the per-member
/// failures were never persisted anywhere a later screen could read them.
pub async fn set_member_cleanup(
    conn: &DatabaseConnection,
    batch_id: i32,
    task_id: i32,
    result: MemberCleanupResult,
    error: Option<&str>,
) -> Result<(), DbError> {
    let Some(member) = work_task_batch_member::Entity::find()
        .filter(work_task_batch_member::Column::BatchId.eq(batch_id))
        .filter(work_task_batch_member::Column::TaskId.eq(task_id))
        .one(conn)
        .await?
    else {
        return Ok(());
    };
    let mut active: work_task_batch_member::ActiveModel = member.into();
    active.cleanup_result = Set(Some(result));
    active.cleanup_error = Set(error.map(str::to_string));
    active.update(conn).await?;
    Ok(())
}

/// Soft-delete a batch. The member tasks are left alone: they are ordinary
/// tasks whose own lifecycle (and worktrees) outlive the grouping, and deleting
/// a grouping must not silently destroy work.
pub async fn soft_delete(conn: &DatabaseConnection, batch_id: i32) -> Result<(), DbError> {
    let batch = get_model(conn, batch_id).await?;
    let mut active: work_task_batch::ActiveModel = batch.into();
    active.deleted_at = Set(Some(Utc::now()));
    active.update(conn).await?;
    Ok(())
}

/// Task ids of a batch in slot order — the launch order an aggregate start
/// follows.
pub async fn member_task_ids(
    conn: &DatabaseConnection,
    batch_id: i32,
) -> Result<Vec<i32>, DbError> {
    Ok(member_models(conn, batch_id)
        .await?
        .into_iter()
        .map(|m| m.task_id)
        .collect())
}

/// Slot index of each member task, for building per-member results.
pub async fn slot_of_tasks(
    conn: &DatabaseConnection,
    batch_id: i32,
) -> Result<std::collections::BTreeMap<i32, i32>, DbError> {
    Ok(member_models(conn, batch_id)
        .await?
        .into_iter()
        .map(|m| (m.task_id, m.slot_index))
        .collect())
}

/// Members of a spec, as the create path sees them. Exposed for tests.
pub fn spec_member_titles(spec: &WorkTaskBatchSpec) -> Vec<&str> {
    spec.members
        .iter()
        .map(|m: &WorkTaskBatchMemberSpec| m.title.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use sea_orm::IntoActiveModel;

    const BASE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn base() -> ResolvedBase {
        ResolvedBase {
            sha: BASE.to_string(),
            branch: "main".to_string(),
        }
    }

    fn member(title: &str) -> WorkTaskBatchMemberSpec {
        WorkTaskBatchMemberSpec {
            title: title.to_string(),
            config: serde_json::json!({
                "display_text": "do the thing",
                "prompt_blocks": [{ "type": "text", "text": "do the thing" }],
            }),
            label: None,
            profile_snapshot: None,
        }
    }

    fn spec(folder_id: i32, titles: &[&str]) -> WorkTaskBatchSpec {
        WorkTaskBatchSpec {
            folder_id,
            title: "compare three agents".to_string(),
            members: titles.iter().map(|t| member(t)).collect(),
            failure_policy: None,
            max_concurrent: None,
            owner_extension: None,
            metadata: None,
            allow_dirty: false,
        }
    }

    /// Force a member's status without going through the engine. The engine owns
    /// these transitions in production; the projection under test only reads
    /// them, so the tests write them directly rather than booting an engine.
    async fn set_status(db: &crate::db::AppDatabase, task_id: i32, status: WorkTaskStatus) {
        let task = work_task::Entity::find_by_id(task_id)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("task");
        let mut active = task.into_active_model();
        active.status = Set(status);
        active.update(&db.conn).await.expect("set status");
    }

    #[tokio::test]
    async fn create_pins_one_base_onto_every_member() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-base").await;

        let info = create(&db.conn, &spec(folder_id, &["a", "b", "c"]), base())
            .await
            .expect("create");

        assert_eq!(info.base_sha, BASE);
        assert_eq!(info.base_branch, "main");
        assert_eq!(info.status, WorkTaskBatchStatus::Created);
        assert_eq!(info.members.len(), 3);

        // Slots are dense and ordered, and every member is an ordinary todo task.
        for (i, m) in info.members.iter().enumerate() {
            assert_eq!(m.slot_index, i as i32);
            assert_eq!(m.task_status, Some(WorkTaskStatus::Todo));
            assert!(m.cleanup_result.is_none(), "nothing cleaned up yet");
        }

        // Every member resolves the SAME pinned base — the property the whole
        // type exists for.
        for m in &info.members {
            let pinned = pinned_base_of_task(&db.conn, m.task_id)
                .await
                .expect("lookup")
                .expect("member has a pinned base");
            assert_eq!(pinned, ("main".to_string(), BASE.to_string()));
        }
    }

    // ── adopt: the primitive's second consumer ──────────────────────────────
    //
    // `create` mints members; `adopt` takes rows the user already wrote. These
    // tests pin the eligibility rules, because each refusal is the difference
    // between a batch that means what it says and one whose "shared base" is
    // fiction for some member.

    /// Seed a plain to-do task on `folder_id`, the way the board does.
    async fn seed_todo(
        db: &crate::db::AppDatabase,
        folder_id: i32,
        title: &str,
    ) -> i32 {
        crate::db::service::work_task_service::create(
            &db.conn,
            crate::models::WorkTaskDraft {
                folder_id,
                title: title.to_string(),
                config: serde_json::json!({
                    "display_text": "do the thing",
                    "prompt_blocks": [{ "type": "text", "text": "do the thing" }],
                }),
            },
        )
        .await
        .expect("seed todo")
        .id
    }

    #[tokio::test]
    async fn adopt_groups_existing_todos_under_one_pinned_base() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-adopt").await;
        let a = seed_todo(&db, folder_id, "first").await;
        let b = seed_todo(&db, folder_id, "second").await;

        let info = adopt(&db.conn, folder_id, "run these two", &[a, b], base())
            .await
            .expect("adopt");

        assert_eq!(info.base_sha, BASE);
        assert_eq!(info.status, WorkTaskBatchStatus::Created);
        // No owner: a plain bulk operation, which is precisely what makes this a
        // second consumer rather than the app in disguise.
        assert!(info.owner_extension.is_none());
        // The tasks keep their own titles — nothing was renamed or recreated.
        assert_eq!(
            info.members
                .iter()
                .map(|m| m.task_title.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        // Selection order is the slot order.
        assert_eq!(
            info.members.iter().map(|m| m.task_id).collect::<Vec<_>>(),
            vec![a, b]
        );
        // And both now resolve the shared base the engine will branch them from.
        for id in [a, b] {
            assert_eq!(
                pinned_base_of_task(&db.conn, id).await.unwrap(),
                Some(("main".to_string(), BASE.to_string()))
            );
        }
    }

    /// A task whose worktree exists already started somewhere. A batch claiming
    /// to pin its base would be recording a commit it does not have.
    #[tokio::test]
    async fn adopt_refuses_a_task_that_already_has_a_worktree() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-adopt-wt").await;
        let a = seed_todo(&db, folder_id, "has a worktree").await;
        let wt = seed_folder(&db, "/tmp/batch-adopt-wt-task").await;
        crate::db::service::work_task_service::attach_worktree(
            &db.conn,
            a,
            wt,
            "main",
            "deadbeef",
            "task/1",
        )
        .await
        .expect("attach");

        let err = adopt(&db.conn, folder_id, "t", &[a], base())
            .await
            .expect_err("a task with a worktree cannot be pinned");
        assert!(
            err.to_string().contains("already has a worktree"),
            "the refusal must name the real cause, got: {err}"
        );
        // And nothing was written.
        assert!(list(&db.conn, Some(folder_id)).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn adopt_refuses_a_task_that_is_not_todo() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-adopt-status").await;
        let a = seed_todo(&db, folder_id, "already running").await;
        set_status(&db, a, WorkTaskStatus::Running).await;

        let err = adopt(&db.conn, folder_id, "t", &[a], base())
            .await
            .expect_err("a running task's start is behind it");
        assert!(err.to_string().contains("running"), "got: {err}");
    }

    #[tokio::test]
    async fn adopt_refuses_a_task_from_another_project() {
        let db = fresh_in_memory_db().await;
        let mine = seed_folder(&db, "/tmp/batch-adopt-mine").await;
        let theirs = seed_folder(&db, "/tmp/batch-adopt-theirs").await;
        let a = seed_todo(&db, mine, "mine").await;
        let b = seed_todo(&db, theirs, "theirs").await;

        let err = adopt(&db.conn, mine, "t", &[a, b], base())
            .await
            .expect_err("a batch shares one repository's commit");
        assert!(err.to_string().contains("another project"), "got: {err}");
        assert!(list(&db.conn, Some(mine)).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn adopt_refuses_a_task_already_in_a_batch() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-adopt-twice").await;
        let a = seed_todo(&db, folder_id, "first").await;
        adopt(&db.conn, folder_id, "one", &[a], base())
            .await
            .expect("first adopt");

        let err = adopt(&db.conn, folder_id, "two", &[a], base())
            .await
            .expect_err("a task belongs to at most one batch");
        assert!(err.to_string().contains("already belongs"), "got: {err}");
        // Exactly one batch exists — the refusal wrote nothing.
        assert_eq!(list(&db.conn, Some(folder_id)).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn adopt_refuses_an_empty_or_duplicated_or_oversized_selection() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-adopt-selection").await;
        let a = seed_todo(&db, folder_id, "first").await;

        assert!(adopt(&db.conn, folder_id, "t", &[], base()).await.is_err());
        assert!(adopt(&db.conn, folder_id, "  ", &[a], base()).await.is_err());

        // The same task twice would take two slots for one row and then trip the
        // unique index mid-transaction; reported as itself instead.
        let err = adopt(&db.conn, folder_id, "t", &[a, a], base())
            .await
            .expect_err("a repeated selection is a mistake, not a two-member batch");
        assert!(err.to_string().contains("more than once"), "got: {err}");

        let many: Vec<i32> = (0..=MAX_MEMBERS as i32).collect();
        assert!(adopt(&db.conn, folder_id, "t", &many, base()).await.is_err());
    }

    /// The adopted task's own timeline records that its start was decided
    /// elsewhere — the same courtesy `create` extends to its members.
    #[tokio::test]
    async fn adopt_records_the_pinned_base_on_each_task_timeline() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-adopt-event").await;
        let a = seed_todo(&db, folder_id, "first").await;
        let info = adopt(&db.conn, folder_id, "t", &[a], base())
            .await
            .expect("adopt");

        let events =
            crate::db::service::work_task_service::list_events(&db.conn, a, 50)
                .await
                .unwrap();
        let joined = events
            .iter()
            .find(|e| e.kind == "batch_joined")
            .expect("a batch_joined event");
        let payload = joined.payload.as_ref().expect("payload");
        assert_eq!(payload["batch_id"], info.id);
        assert_eq!(payload["base_sha"], BASE);
        assert_eq!(payload["base_branch"], "main");
    }

    /// An adopted batch answers to the same aggregates and the same projection as
    /// a created one — there is one primitive, not two.
    #[tokio::test]
    async fn an_adopted_batch_projects_and_cancels_like_any_other() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-adopt-parity").await;
        let a = seed_todo(&db, folder_id, "first").await;
        let b = seed_todo(&db, folder_id, "second").await;
        let info = adopt(&db.conn, folder_id, "t", &[a, b], base())
            .await
            .expect("adopt");

        set_status(&db, a, WorkTaskStatus::Running).await;
        assert_eq!(
            recompute_status(&db.conn, info.id).await.unwrap(),
            WorkTaskBatchStatus::Running
        );

        mark_canceled(&db.conn, info.id).await.unwrap();
        set_status(&db, a, WorkTaskStatus::Done).await;
        set_status(&db, b, WorkTaskStatus::Done).await;
        assert_eq!(
            recompute_status(&db.conn, info.id).await.unwrap(),
            WorkTaskBatchStatus::Canceled,
            "an adopted batch lost the cancel gate a created one has"
        );
    }

    /// The applied profile is READ FROM THE ENGINE'S AUDIT TRAIL, not from the
    /// member row.
    ///
    /// This test exists because the first cut of the member card read
    /// `profile_snapshot` for the applied values — a field written at creation
    /// time, when no agent process exists and therefore no applied value does
    /// either. The card's "actually in effect" section was silently always empty.
    /// The applied values can only come from the engine, after a launch.
    #[tokio::test]
    async fn the_applied_profile_comes_from_the_engines_own_audit_event() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-applied").await;
        let info = create(&db.conn, &spec(folder_id, &["a"]), base())
            .await
            .unwrap();
        let task_id = info.members[0].task_id;

        // Before any launch there is no such event, and the field is absent
        // rather than an empty object the UI would render as "no gaps".
        assert!(
            get(&db.conn, info.id).await.unwrap().members[0]
                .applied_profile
                .is_none(),
            "an unlaunched member reported an applied profile"
        );

        // The engine writes this on every launch.
        crate::db::service::work_task_service::record_event(
            &db.conn,
            task_id,
            "config_effective",
            "engine",
            Some(serde_json::json!({
                "agent": "codex",
                "profile": {
                    "agent_id": "Codex",
                    "applied": { "effort": "medium" },
                    "warnings": ["effort 'high' is not in the known vocabulary"],
                },
            })),
        )
        .await
        .unwrap();

        let member = &get(&db.conn, info.id).await.unwrap().members[0];
        let applied = member.applied_profile.as_ref().expect("applied profile");
        assert_eq!(applied["applied"]["effort"], "medium");
        assert_eq!(
            applied["warnings"][0],
            "effort 'high' is not in the known vocabulary",
            "the gap between requested and applied was lost"
        );
    }

    /// A retry launches a new generation with possibly different values; the card
    /// must report the current one, not the first.
    #[tokio::test]
    async fn the_applied_profile_follows_the_newest_launch() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-applied-retry").await;
        let info = create(&db.conn, &spec(folder_id, &["a"]), base())
            .await
            .unwrap();
        let task_id = info.members[0].task_id;

        for model in ["gpt-5", "gpt-5-codex"] {
            crate::db::service::work_task_service::record_event(
                &db.conn,
                task_id,
                "config_effective",
                "engine",
                Some(serde_json::json!({
                    "profile": { "applied": { "model": model } },
                })),
            )
            .await
            .unwrap();
        }

        let member = &get(&db.conn, info.id).await.unwrap().members[0];
        assert_eq!(
            member.applied_profile.as_ref().unwrap()["applied"]["model"],
            "gpt-5-codex",
            "a stale generation's profile outlived the retry"
        );
    }

    /// A payload without a usable profile reads as "not launched yet". Evidence
    /// for a comparison must never be the reason the view fails to render.
    #[tokio::test]
    async fn a_profileless_config_event_reads_as_no_applied_profile() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-applied-empty").await;
        let info = create(&db.conn, &spec(folder_id, &["a"]), base())
            .await
            .unwrap();
        let task_id = info.members[0].task_id;

        // An older build wrote this event without a `profile` field.
        crate::db::service::work_task_service::record_event(
            &db.conn,
            task_id,
            "config_effective",
            "engine",
            Some(serde_json::json!({ "agent": "codex", "mode": null })),
        )
        .await
        .unwrap();

        assert!(get(&db.conn, info.id).await.unwrap().members[0]
            .applied_profile
            .is_none());
    }

    #[tokio::test]
    async fn create_refuses_empty_and_oversized_and_untitled_membership() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-validate").await;

        assert!(create(&db.conn, &spec(folder_id, &[]), base()).await.is_err());

        let mut untitled = spec(folder_id, &["ok"]);
        untitled.members.push(member("   "));
        assert!(create(&db.conn, &untitled, base()).await.is_err());

        let mut blank_title = spec(folder_id, &["ok"]);
        blank_title.title = "  ".into();
        assert!(create(&db.conn, &blank_title, base()).await.is_err());

        let too_many: Vec<&str> = (0..=MAX_MEMBERS).map(|_| "m").collect();
        assert!(create(&db.conn, &spec(folder_id, &too_many), base())
            .await
            .is_err());
    }

    /// A batch is a grouping over a project, not over a worktree: a member rooted
    /// in a worktree would nest worktrees when the engine launched it.
    #[tokio::test]
    async fn create_refuses_a_worktree_folder() {
        let db = fresh_in_memory_db().await;
        let root = seed_folder(&db, "/tmp/batch-root").await;
        let wt = seed_folder(&db, "/tmp/batch-root-task-1").await;
        let row = folder::Entity::find_by_id(wt)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut active = row.into_active_model();
        active.parent_id = Set(Some(root));
        active.update(&db.conn).await.unwrap();

        assert!(create(&db.conn, &spec(wt, &["a"]), base()).await.is_err());
    }

    /// Nothing is half-created: a member that cannot be written rolls the batch
    /// back with it, so the user is never left with orphan tasks on a base no
    /// row records.
    #[tokio::test]
    async fn a_rejected_member_leaves_no_batch_behind() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-atomic").await;

        let before = list(&db.conn, Some(folder_id)).await.unwrap().len();
        let mut bad = spec(folder_id, &["good"]);
        bad.members.push(member(""));
        assert!(create(&db.conn, &bad, base()).await.is_err());

        assert_eq!(list(&db.conn, Some(folder_id)).await.unwrap().len(), before);
        assert!(
            work_task::Entity::find()
                .filter(work_task::Column::FolderId.eq(folder_id))
                .all(&db.conn)
                .await
                .unwrap()
                .is_empty(),
            "a rolled-back batch left member tasks behind"
        );
    }

    #[tokio::test]
    async fn the_projection_walks_created_running_review_settled() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-projection").await;
        let info = create(&db.conn, &spec(folder_id, &["a", "b"]), base())
            .await
            .unwrap();
        let (a, b) = (info.members[0].task_id, info.members[1].task_id);

        // All todo → created.
        assert_eq!(
            recompute_status(&db.conn, info.id).await.unwrap(),
            WorkTaskBatchStatus::Created
        );

        // One live → running.
        set_status(&db, a, WorkTaskStatus::Running).await;
        assert_eq!(
            recompute_status(&db.conn, info.id).await.unwrap(),
            WorkTaskBatchStatus::Running
        );

        // Nothing live, one awaiting a decision → review. NOT `created`: a round
        // whose agents have all finished has not "never started".
        set_status(&db, a, WorkTaskStatus::Review).await;
        set_status(&db, b, WorkTaskStatus::Failed).await;
        assert_eq!(
            recompute_status(&db.conn, info.id).await.unwrap(),
            WorkTaskBatchStatus::Review
        );
        assert!(
            get(&db.conn, info.id).await.unwrap().settled_at.is_none(),
            "a batch still awaiting a decision must not carry a finish line"
        );

        // Everything terminal → settled, stamped once.
        set_status(&db, a, WorkTaskStatus::Done).await;
        assert_eq!(
            recompute_status(&db.conn, info.id).await.unwrap(),
            WorkTaskBatchStatus::Settled
        );
        let settled_at = get(&db.conn, info.id).await.unwrap().settled_at;
        assert!(settled_at.is_some());
        recompute_status(&db.conn, info.id).await.unwrap();
        assert_eq!(
            get(&db.conn, info.id).await.unwrap().settled_at,
            settled_at,
            "an idempotent recompute moved the finish line"
        );
    }

    /// The batch-level counterpart of the engine's `run_seq` guard, and a direct
    /// answer to the reviewed defect where cancellation was followed by members
    /// being written back to a live state: once the user cancels, NOTHING a
    /// member reports afterwards can present the batch as a normal completion.
    #[tokio::test]
    async fn a_canceled_batch_cannot_be_repainted_by_late_member_events() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-cancel-gate").await;
        let info = create(&db.conn, &spec(folder_id, &["a", "b"]), base())
            .await
            .unwrap();
        let (a, b) = (info.members[0].task_id, info.members[1].task_id);

        set_status(&db, a, WorkTaskStatus::Running).await;
        mark_canceled(&db.conn, info.id).await.unwrap();
        assert_eq!(
            get(&db.conn, info.id).await.unwrap().status,
            WorkTaskBatchStatus::Canceled
        );

        // A member that was mid-setup reports a normal finish afterwards.
        set_status(&db, a, WorkTaskStatus::Review).await;
        set_status(&db, b, WorkTaskStatus::Done).await;
        assert_eq!(
            recompute_status(&db.conn, info.id).await.unwrap(),
            WorkTaskBatchStatus::Canceled,
            "a late member event moved a canceled batch"
        );

        // Idempotent, and still one-way.
        mark_canceled(&db.conn, info.id).await.unwrap();
        assert_eq!(
            get(&db.conn, info.id).await.unwrap().status,
            WorkTaskBatchStatus::Canceled
        );
    }

    /// The write-side half of the cancel gate.
    ///
    /// [`recompute_status`] checks for a canceled batch when it reads, but the
    /// read and the write are different moments — a cancel landing between them
    /// would be overwritten by a projection computed before it existed, and the
    /// batch would report a normal completion after the user canceled it. The
    /// UPDATE therefore carries the condition too.
    ///
    /// This test reproduces that interleaving deterministically by driving the
    /// two halves by hand: compute the projection against a live batch, cancel,
    /// then let the stale projection try to land.
    #[tokio::test]
    async fn a_cancel_landing_mid_recompute_still_wins() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-cas").await;
        let info = create(&db.conn, &spec(folder_id, &["a", "b"]), base())
            .await
            .unwrap();
        let (a, b) = (info.members[0].task_id, info.members[1].task_id);

        // The batch is live, and this is what a recompute would have read.
        set_status(&db, a, WorkTaskStatus::Running).await;
        recompute_status(&db.conn, info.id).await.unwrap();
        assert_eq!(
            get(&db.conn, info.id).await.unwrap().status,
            WorkTaskBatchStatus::Running
        );

        // ── the interleaving ──
        // 1. A member settles; a recompute pass would now compute `settled`.
        set_status(&db, a, WorkTaskStatus::Done).await;
        set_status(&db, b, WorkTaskStatus::Done).await;
        // 2. The user cancels before that pass writes.
        mark_canceled(&db.conn, info.id).await.unwrap();
        // 3. The pass writes. Its condition must refuse.
        let reported = recompute_status(&db.conn, info.id).await.unwrap();

        assert_eq!(
            reported,
            WorkTaskBatchStatus::Canceled,
            "the pass reported its own stale projection instead of the row"
        );
        assert_eq!(
            get(&db.conn, info.id).await.unwrap().status,
            WorkTaskBatchStatus::Canceled,
            "a stale projection overwrote the user's cancellation"
        );
        assert!(
            get(&db.conn, info.id).await.unwrap().settled_at.is_none(),
            "a canceled batch was stamped with a completion time"
        );
    }

    /// A retry after everything settled reopens the batch — and the stale finish
    /// line has to go with it, or the UI shows a completed batch that is running.
    #[tokio::test]
    async fn reopening_a_settled_batch_clears_its_finish_line() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-reopen").await;
        let info = create(&db.conn, &spec(folder_id, &["a"]), base())
            .await
            .unwrap();
        let a = info.members[0].task_id;

        set_status(&db, a, WorkTaskStatus::Done).await;
        recompute_status(&db.conn, info.id).await.unwrap();
        assert!(get(&db.conn, info.id).await.unwrap().settled_at.is_some());

        // A requeue puts the member back in todo.
        set_status(&db, a, WorkTaskStatus::Todo).await;
        assert_eq!(
            recompute_status(&db.conn, info.id).await.unwrap(),
            WorkTaskBatchStatus::Created
        );
        assert!(
            get(&db.conn, info.id).await.unwrap().settled_at.is_none(),
            "a reopened batch kept the finish line it no longer has"
        );
    }

    /// A member's row can be deleted while its batch lives on. It will never
    /// report again, so counting it as outstanding would strand the batch in
    /// `running` for good.
    #[tokio::test]
    async fn a_deleted_member_does_not_strand_the_batch() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-deleted-member").await;
        let info = create(&db.conn, &spec(folder_id, &["a", "b"]), base())
            .await
            .unwrap();
        let (a, b) = (info.members[0].task_id, info.members[1].task_id);

        set_status(&db, a, WorkTaskStatus::Done).await;
        work_task::Entity::delete_by_id(b)
            .exec(&db.conn)
            .await
            .unwrap();

        assert_eq!(
            recompute_status(&db.conn, info.id).await.unwrap(),
            WorkTaskBatchStatus::Settled
        );
    }

    /// The cleanup ledger. This test is the schema-level counterpart of the
    /// reviewed failure: a UI reported cleanup success while every worktree
    /// stayed on disk, because the per-member failures were collected and
    /// dropped. A failure that survives in the row can be shown and retried.
    #[tokio::test]
    async fn a_failed_cleanup_is_remembered_with_its_reason() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-cleanup").await;
        let info = create(&db.conn, &spec(folder_id, &["a", "b", "c"]), base())
            .await
            .unwrap();
        let (a, b, c) = (
            info.members[0].task_id,
            info.members[1].task_id,
            info.members[2].task_id,
        );

        set_member_cleanup(&db.conn, info.id, a, MemberCleanupResult::Succeeded, None)
            .await
            .unwrap();
        set_member_cleanup(
            &db.conn,
            info.id,
            b,
            MemberCleanupResult::Failed,
            Some("worktree is locked by another process"),
        )
        .await
        .unwrap();
        set_member_cleanup(&db.conn, info.id, c, MemberCleanupResult::Blocked, None)
            .await
            .unwrap();

        let members = get(&db.conn, info.id).await.unwrap().members;
        assert_eq!(
            members[0].cleanup_result,
            Some(MemberCleanupResult::Succeeded)
        );
        assert_eq!(members[1].cleanup_result, Some(MemberCleanupResult::Failed));
        assert_eq!(
            members[1].cleanup_error.as_deref(),
            Some("worktree is locked by another process"),
            "the reason a retry needs was not kept"
        );
        assert_eq!(members[2].cleanup_result, Some(MemberCleanupResult::Blocked));

        // A later success supersedes the failure — the retry has to be able to
        // clear the flag it offered.
        set_member_cleanup(&db.conn, info.id, b, MemberCleanupResult::Succeeded, None)
            .await
            .unwrap();
        let members = get(&db.conn, info.id).await.unwrap().members;
        assert_eq!(
            members[1].cleanup_result,
            Some(MemberCleanupResult::Succeeded)
        );
        assert!(members[1].cleanup_error.is_none());
    }

    /// Deleting the grouping must not destroy the work. The member tasks and
    /// their worktrees outlive it, and they go back to being ordinary tasks —
    /// which means they stop resolving a pinned base.
    #[tokio::test]
    async fn deleting_a_batch_keeps_its_members_and_unpins_them() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-delete").await;
        let info = create(&db.conn, &spec(folder_id, &["a", "b"]), base())
            .await
            .unwrap();
        let a = info.members[0].task_id;

        soft_delete(&db.conn, info.id).await.unwrap();

        assert!(get(&db.conn, info.id).await.is_err(), "still readable");
        assert!(
            list(&db.conn, Some(folder_id)).await.unwrap().is_empty(),
            "a deleted batch is still listed"
        );
        assert!(
            work_task::Entity::find_by_id(a)
                .one(&db.conn)
                .await
                .unwrap()
                .is_some(),
            "deleting a grouping destroyed a member task"
        );
        assert!(
            pinned_base_of_task(&db.conn, a).await.unwrap().is_none(),
            "a member of a deleted batch still resolves a pinned base"
        );
    }

    /// A task outside any batch resolves no pinned base and no batch id — the
    /// engine's fast path for the overwhelming majority of tasks.
    #[tokio::test]
    async fn a_plain_task_belongs_to_no_batch() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-none").await;
        let plain = crate::db::service::work_task_service::create(
            &db.conn,
            crate::models::WorkTaskDraft {
                folder_id,
                title: "standalone".into(),
                config: serde_json::json!({
                    "display_text": "x",
                    "prompt_blocks": [{ "type": "text", "text": "x" }],
                }),
            },
        )
        .await
        .unwrap();

        assert!(batch_of_task(&db.conn, plain.id).await.unwrap().is_none());
        assert!(pinned_base_of_task(&db.conn, plain.id)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn owner_extension_and_metadata_round_trip_untouched() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-owner").await;
        let mut s = spec(folder_id, &["a"]);
        s.owner_extension = Some("example.owner".into());
        s.metadata = Some(serde_json::json!({ "layout": "grid", "slots": 4 }));
        s.max_concurrent = Some(2);
        s.failure_policy = Some(WorkTaskBatchFailurePolicy::FailFast);
        s.members[0].label = Some("slot A".into());
        s.members[0].profile_snapshot = Some(serde_json::json!({
            "agent_id": "codex",
            "applied": { "effort": "high" },
        }));

        let info = create(&db.conn, &s, base()).await.unwrap();
        let read = get(&db.conn, info.id).await.unwrap();

        // Core carries the owner's vocabulary without interpreting it.
        assert_eq!(read.owner_extension.as_deref(), Some("example.owner"));
        assert_eq!(read.metadata.as_ref().unwrap()["layout"], "grid");
        assert_eq!(read.max_concurrent, Some(2));
        assert_eq!(read.failure_policy, WorkTaskBatchFailurePolicy::FailFast);
        assert_eq!(read.members[0].label.as_deref(), Some("slot A"));
        assert_eq!(
            read.members[0].profile_snapshot.as_ref().unwrap()["applied"]["effort"],
            "high",
            "the launch snapshot that makes a comparison honest was lost"
        );
    }

    /// `max_concurrent: 0` means "no cap" everywhere else in this codebase; it
    /// must not become a batch that can never launch anything.
    #[tokio::test]
    async fn a_zero_concurrency_cap_reads_as_no_cap() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-zero").await;
        let mut s = spec(folder_id, &["a"]);
        s.max_concurrent = Some(0);
        let info = create(&db.conn, &s, base()).await.unwrap();
        assert_eq!(info.max_concurrent, None);
    }

    /// The engine's lookup is by task id and must stay unambiguous: one task
    /// cannot be a member of two batches pinning different commits.
    #[tokio::test]
    async fn a_task_cannot_join_two_batches() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-unique").await;
        let info = create(&db.conn, &spec(folder_id, &["a"]), base())
            .await
            .unwrap();
        let other = create(&db.conn, &spec(folder_id, &["b"]), base())
            .await
            .unwrap();

        let duplicate = work_task_batch_member::ActiveModel {
            id: NotSet,
            batch_id: Set(other.id),
            task_id: Set(info.members[0].task_id),
            slot_index: Set(0),
            label: Set(None),
            profile_snapshot: Set(None),
            cleanup_result: Set(None),
            cleanup_error: Set(None),
            created_at: Set(Utc::now()),
        }
        .insert(&db.conn)
        .await;
        assert!(
            duplicate.is_err(),
            "the unique index let a task join a second batch"
        );
    }

    /// Members are created in slot order and keep it on read, whatever the
    /// database returns them in — the report and the grid both depend on a
    /// stable order.
    #[tokio::test]
    async fn members_read_back_in_slot_order() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-order").await;
        let titles = ["first", "second", "third", "fourth"];
        let info = create(&db.conn, &spec(folder_id, &titles), base())
            .await
            .unwrap();

        let read = get(&db.conn, info.id).await.unwrap();
        let read_titles: Vec<&str> = read.members.iter().map(|m| m.task_title.as_str()).collect();
        assert_eq!(read_titles, titles);
        let slots: Vec<i32> = read.members.iter().map(|m| m.slot_index).collect();
        assert_eq!(slots, vec![0, 1, 2, 3]);
    }

    /// The create path writes the pinned base onto each member's own timeline:
    /// a task whose starting commit was decided elsewhere should say so where it
    /// is read.
    #[tokio::test]
    async fn each_member_records_the_pinned_base_on_its_timeline() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/batch-event").await;
        let info = create(&db.conn, &spec(folder_id, &["a"]), base())
            .await
            .unwrap();

        let events = crate::db::service::work_task_service::list_events(
            &db.conn,
            info.members[0].task_id,
            50,
        )
        .await
        .unwrap();
        let created = events
            .iter()
            .find(|e| e.kind == "created")
            .expect("a created event");
        let payload = created.payload.as_ref().expect("payload");
        assert_eq!(payload["batch_id"], info.id);
        assert_eq!(payload["base_sha"], BASE);
        assert_eq!(payload["base_branch"], "main");
        assert_eq!(payload["slot_index"], 0);
    }

    #[tokio::test]
    async fn live_and_terminal_partition_the_pipeline() {
        // Every status is in exactly one of {live, terminal, neither}; `review`
        // and `todo` are deliberately in neither, and the projection reads that
        // gap as "the user's turn" / "not started".
        for s in [
            WorkTaskStatus::Queued,
            WorkTaskStatus::Preparing,
            WorkTaskStatus::Running,
            WorkTaskStatus::AwaitingInput,
            WorkTaskStatus::Merging,
        ] {
            assert!(is_live(s), "{s:?} should be live");
            assert!(!is_terminal(s), "{s:?} cannot be both");
        }
        for s in [
            WorkTaskStatus::Done,
            WorkTaskStatus::Failed,
            WorkTaskStatus::Canceled,
        ] {
            assert!(is_terminal(s), "{s:?} should be terminal");
            assert!(!is_live(s), "{s:?} cannot be both");
        }
        for s in [WorkTaskStatus::Todo, WorkTaskStatus::Review] {
            assert!(!is_live(s) && !is_terminal(s), "{s:?} belongs to neither");
        }
    }
}
