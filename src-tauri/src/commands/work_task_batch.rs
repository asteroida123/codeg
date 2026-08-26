//! `WorkTaskBatch` commands — create, read, and the three aggregates.
//!
//! Same split as `commands::work_task`: `*_core` is mode-agnostic and shared by
//! the Tauri wrappers and the Axum handlers, and anything that launches,
//! cancels, or removes a worktree routes through the process-global task engine.
//!
//! The one thing that happens *here* rather than in the service is reading git:
//! resolving the folder's HEAD into the commit the batch pins, and deciding
//! whether the working tree is in a state where that pin means what the user
//! will assume it means.

use crate::app_error::AppCommandError;
use crate::commands::folders::{get_folder_core, git_is_clean, resolve_git_head};
use crate::db::error::DbError;
use crate::db::service::work_task_batch_service::{self, ResolvedBase};
use crate::db::AppDatabase;
use crate::models::{
    BatchCleanupOutcome, BatchMemberOutcome, WorkTaskBatchInfo, WorkTaskBatchSpec,
};
use crate::web::event_bridge::{
    emit_event, EventEmitter, WorkTaskBatchChange, WORK_TASK_BATCH_CHANGED_EVENT,
};
use crate::work_task::git as task_git;

fn engine() -> Result<std::sync::Arc<crate::work_task::TaskEngine>, DbError> {
    crate::work_task::engine()
        .ok_or_else(|| DbError::Validation("task engine not running".to_string()))
}

// ── shared business logic (both modes) ──────────────────────────────────────

/// Resolve the commit a new batch pins, from the project folder's HEAD.
///
/// Three refusals, none of which can be worked around by the caller, and all
/// three of which exist because the alternative was found doing damage:
///
/// - **Not a branch** (detached HEAD): a member's work would have nowhere to
///   merge back to. The same refusal a standalone task already makes.
/// - **No commits yet** (unborn HEAD): there is no commit to branch from. The
///   reviewed implementation "solved" this by running
///   `git commit --allow-empty` to seed one — which committed whatever the user
///   had staged, including a `.env`, under a hardcoded author. Nothing in this
///   path writes to the repository; an empty repository is told to make its
///   first commit itself.
/// - **Dirty tree**, unless the caller passes `allow_dirty`: members branch from
///   the recorded commit, so uncommitted work is simply absent from every
///   member's starting tree. Refusing by default makes that a decision instead
///   of a discovery. `allow_dirty` proceeds — it never stages, commits, or
///   stashes anything.
///
/// "Dirty" here means **modified tracked files**, via the repository's existing
/// [`git_is_clean`] convention. Untracked files do not refuse a batch: nearly
/// every working tree has some (build output, logs, local notes), and a
/// standalone task's worktree does not inherit them either — a batch that were
/// stricter than a single task would be inconsistent for no gain in safety.
async fn resolve_base(path: &str, allow_dirty: bool) -> Result<ResolvedBase, AppCommandError> {
    let head = resolve_git_head(path).await?;
    if !head.is_repo {
        return Err(AppCommandError::invalid_input(
            "a batch needs a git repository — this folder is not one",
        ));
    }
    let Some(branch) = head.branch else {
        return Err(AppCommandError::invalid_input(
            "the project folder is not on a branch (detached HEAD?) — check one out, then \
             create the batch",
        ));
    };
    // Unborn HEAD surfaces here: `rev-parse HEAD` fails on a branch with no
    // commits. Reported as itself rather than as a git error, and emphatically
    // not repaired.
    let sha = task_git::rev_parse(path, "HEAD").await.map_err(|_| {
        AppCommandError::invalid_input(
            "this repository has no commits yet — make the first commit, then create the batch \
             (codeg will not commit on your behalf)",
        )
    })?;

    if !allow_dirty {
        // A git error reads as "clean" here, matching `git_is_clean`'s own
        // contract: this check is a courtesy warning about work the user would
        // not see in the members, not a safety gate — nothing downstream is
        // unsafe when the tree is dirty, it is merely surprising.
        let clean = git_is_clean(path.to_string()).await.unwrap_or(true);
        if !clean {
            return Err(AppCommandError::invalid_input(
                "the project folder has uncommitted changes. Every member branches from the \
                 recorded commit, so those changes would be absent from all of them — commit or \
                 stash them, or create the batch with `allow_dirty` to proceed knowingly.",
            ));
        }
    }

    Ok(ResolvedBase { sha, branch })
}

pub async fn work_task_batch_create_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    spec: WorkTaskBatchSpec,
) -> Result<WorkTaskBatchInfo, AppCommandError> {
    let folder = get_folder_core(db, spec.folder_id).await?;
    let base = resolve_base(&folder.path, spec.allow_dirty).await?;
    let info = work_task_batch_service::create(&db.conn, &spec, base)
        .await
        .map_err(AppCommandError::from)?;
    emit_event(
        emitter,
        WORK_TASK_BATCH_CHANGED_EVENT,
        WorkTaskBatchChange::Upsert { id: info.id },
    );
    // The members are ordinary new tasks — tell the board so they appear beside
    // every other task rather than only inside the batch view.
    emit_event(
        emitter,
        crate::web::event_bridge::WORK_TASK_CHANGED_EVENT,
        crate::web::event_bridge::WorkTaskChange::Refresh,
    );
    Ok(info)
}

pub async fn work_task_batch_list_core(
    db: &AppDatabase,
    folder_id: Option<i32>,
) -> Result<Vec<WorkTaskBatchInfo>, DbError> {
    work_task_batch_service::list(&db.conn, folder_id).await
}

pub async fn work_task_batch_get_core(
    db: &AppDatabase,
    id: i32,
) -> Result<WorkTaskBatchInfo, DbError> {
    work_task_batch_service::get(&db.conn, id).await
}

/// Start every startable member. Per-member outcomes; a member that refuses does
/// not fail the call.
pub async fn work_task_batch_start_core(id: i32) -> Result<Vec<BatchMemberOutcome>, DbError> {
    engine()?.batch_start(id).await.map_err(DbError::Validation)
}

/// Cancel every cancelable member. The batch is marked canceled before any
/// member is touched, so a late event cannot present it as completed.
pub async fn work_task_batch_cancel_core(id: i32) -> Result<Vec<BatchMemberOutcome>, DbError> {
    engine()?.batch_cancel(id).await.map_err(DbError::Validation)
}

/// Remove the members' worktrees and branches, keeping `keep_task_ids`.
///
/// Returns one result per attempted member — `succeeded`, `failed`, or
/// `blocked` — each already persisted on its member row. Callers must render
/// these per member; a single "cleanup done" on top of this call would be the
/// exact false success this API is shaped to prevent.
pub async fn work_task_batch_cleanup_core(
    id: i32,
    keep_task_ids: Vec<i32>,
) -> Result<Vec<BatchCleanupOutcome>, DbError> {
    engine()?
        .batch_cleanup(id, &keep_task_ids)
        .await
        .map_err(DbError::Validation)
}

/// Soft-delete the grouping. Member tasks and their worktrees are untouched:
/// they are ordinary tasks, and dropping a grouping must not destroy work.
pub async fn work_task_batch_delete_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    id: i32,
) -> Result<(), DbError> {
    work_task_batch_service::soft_delete(&db.conn, id).await?;
    emit_event(
        emitter,
        WORK_TASK_BATCH_CHANGED_EVENT,
        WorkTaskBatchChange::Deleted { id },
    );
    Ok(())
}

// ── Tauri wrappers (desktop mode) ───────────────────────────────────────────

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn work_task_batch_create(
    db: tauri::State<'_, AppDatabase>,
    app: tauri::AppHandle,
    spec: WorkTaskBatchSpec,
) -> Result<WorkTaskBatchInfo, AppCommandError> {
    work_task_batch_create_core(&db, &EventEmitter::Tauri(app), spec).await
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn work_task_batch_list(
    db: tauri::State<'_, AppDatabase>,
    folder_id: Option<i32>,
) -> Result<Vec<WorkTaskBatchInfo>, DbError> {
    work_task_batch_list_core(&db, folder_id).await
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn work_task_batch_get(
    db: tauri::State<'_, AppDatabase>,
    id: i32,
) -> Result<WorkTaskBatchInfo, DbError> {
    work_task_batch_get_core(&db, id).await
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn work_task_batch_start(id: i32) -> Result<Vec<BatchMemberOutcome>, DbError> {
    work_task_batch_start_core(id).await
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn work_task_batch_cancel(id: i32) -> Result<Vec<BatchMemberOutcome>, DbError> {
    work_task_batch_cancel_core(id).await
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn work_task_batch_cleanup(
    id: i32,
    keep_task_ids: Vec<i32>,
) -> Result<Vec<BatchCleanupOutcome>, DbError> {
    work_task_batch_cleanup_core(id, keep_task_ids).await
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn work_task_batch_delete(
    db: tauri::State<'_, AppDatabase>,
    app: tauri::AppHandle,
    id: i32,
) -> Result<(), DbError> {
    work_task_batch_delete_core(&db, &EventEmitter::Tauri(app), id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_run(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The headline invariant of this module, and the direct regression for the
    /// reviewed defect: an empty repository is REFUSED, not seeded.
    ///
    /// The implementation that was rejected ran `git commit --allow-empty` here
    /// to manufacture a base. `--allow-empty` permits an empty *result*; it does
    /// not empty the index — so a user who had staged a `.env` and their sources
    /// got them committed as a root commit under a hardcoded author, from a
    /// generic code path that non-batch features also used. This test pins that
    /// nothing in the batch base resolution writes to the repository.
    #[tokio::test]
    async fn an_unborn_head_is_refused_and_nothing_is_committed() {
        let dir = tempfile::tempdir().expect("tempdir");
        git_run(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join(".env"), "TOKEN=secret").expect("write");
        std::fs::write(dir.path().join("src.rs"), "fn main() {}").expect("write");
        git_run(dir.path(), &["add", ".env", "src.rs"]);

        let path = dir.path().to_str().expect("utf-8");
        let err = resolve_base(path, false)
            .await
            .expect_err("an empty repository has no commit to pin");
        let msg = err.to_string();
        assert!(
            msg.contains("no commits yet"),
            "the refusal must name the real cause, got: {msg}"
        );

        // HEAD is still unborn and the staged index is exactly as the user left it.
        let head = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "HEAD"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git");
        assert!(!head.status.success(), "HEAD must remain unborn");

        let staged = std::process::Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git");
        let staged = String::from_utf8_lossy(&staged.stdout);
        assert!(
            staged.contains(".env") && staged.contains("src.rs"),
            "the user's staged index was touched, got: {staged}"
        );

        // And `allow_dirty` is not a back door into seeding one.
        assert!(
            resolve_base(path, true).await.is_err(),
            "allow_dirty must not manufacture a base commit"
        );
    }

    /// A dirty tree is refused by default because the uncommitted work would be
    /// silently absent from every member — and `allow_dirty` proceeds without
    /// staging, committing, or stashing anything.
    #[tokio::test]
    async fn a_dirty_tree_is_refused_by_default_and_never_tidied() {
        let dir = tempfile::tempdir().expect("tempdir");
        git_run(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("a.txt"), "one").expect("write");
        git_run(dir.path(), &["add", "a.txt"]);
        git_run(dir.path(), &["commit", "-q", "-m", "base"]);
        let head_before = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git");
        let head_before = String::from_utf8_lossy(&head_before.stdout).trim().to_string();

        // Uncommitted change on a tracked file.
        std::fs::write(dir.path().join("a.txt"), "two").expect("write");
        let path = dir.path().to_str().expect("utf-8");

        let err = resolve_base(path, false)
            .await
            .expect_err("a dirty tree must be refused by default");
        assert!(
            err.to_string().contains("uncommitted changes"),
            "the refusal must explain itself, got: {err}"
        );

        // Knowingly proceeding works, and pins the committed state.
        let base = resolve_base(path, true)
            .await
            .expect("allow_dirty proceeds");
        assert_eq!(base.sha, head_before);
        assert_eq!(base.branch, "main");

        // Nothing was staged, committed, or stashed on the user's behalf.
        let head_after = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git");
        assert_eq!(
            String::from_utf8_lossy(&head_after.stdout).trim(),
            head_before,
            "resolving a base moved HEAD"
        );
        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git");
        assert!(
            String::from_utf8_lossy(&status.stdout).contains("a.txt"),
            "the user's uncommitted change was tidied away"
        );
        let stash = std::process::Command::new("git")
            .args(["stash", "list"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git");
        assert!(
            String::from_utf8_lossy(&stash.stdout).trim().is_empty(),
            "resolving a base stashed the user's work"
        );
    }

    /// A detached HEAD has no branch for a member's work to merge back to, so the
    /// batch is refused up front rather than at the first merge attempt.
    #[tokio::test]
    async fn a_detached_head_is_refused_with_its_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        git_run(dir.path(), &["init", "-q", "-b", "main"]);
        git_run(dir.path(), &["commit", "-q", "--allow-empty", "-m", "one"]);
        git_run(dir.path(), &["commit", "-q", "--allow-empty", "-m", "two"]);
        git_run(dir.path(), &["checkout", "-q", "--detach", "HEAD~1"]);

        let err = resolve_base(dir.path().to_str().expect("utf-8"), false)
            .await
            .expect_err("a detached HEAD must be refused");
        assert!(
            err.to_string().contains("not on a branch"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn a_non_repository_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = resolve_base(dir.path().to_str().expect("utf-8"), false)
            .await
            .expect_err("a plain directory is not a batch base");
        assert!(err.to_string().contains("git repository"), "got: {err}");
    }

    /// A clean repository resolves to the exact commit HEAD names — and the pin
    /// is a full object id, not a branch name, so a later branch move cannot
    /// redefine where the members started.
    #[tokio::test]
    async fn a_clean_repository_pins_the_exact_commit() {
        let dir = tempfile::tempdir().expect("tempdir");
        git_run(dir.path(), &["init", "-q", "-b", "trunk"]);
        std::fs::write(dir.path().join("a.txt"), "one").expect("write");
        git_run(dir.path(), &["add", "a.txt"]);
        git_run(dir.path(), &["commit", "-q", "-m", "first"]);

        let path = dir.path().to_str().expect("utf-8");
        let base = resolve_base(path, false).await.expect("clean repo resolves");
        assert_eq!(base.branch, "trunk");
        assert_eq!(base.sha.len(), 40, "a pin must be a full object id");

        // Move the branch on. The recorded sha still names the original commit —
        // which is the entire guarantee a batch makes to a comparison.
        std::fs::write(dir.path().join("b.txt"), "two").expect("write");
        git_run(dir.path(), &["add", "b.txt"]);
        git_run(dir.path(), &["commit", "-q", "-m", "second"]);
        let after = resolve_base(path, false).await.expect("resolves again");
        assert_ne!(
            after.sha, base.sha,
            "HEAD moved, so a fresh resolve must see the new commit"
        );

        let subject = std::process::Command::new("git")
            .args(["log", "-1", "--format=%s", &base.sha])
            .current_dir(dir.path())
            .output()
            .expect("spawn git");
        assert_eq!(
            String::from_utf8_lossy(&subject.stdout).trim(),
            "first",
            "the earlier pin no longer names the commit it was taken from"
        );
    }

    /// Untracked files are not "uncommitted changes" for this purpose: they are
    /// not in any commit, so no member could have inherited them either way, and
    /// refusing on them would block most real working trees (build output, local
    /// notes) for no benefit.
    #[tokio::test]
    async fn untracked_files_do_not_block_a_batch() {
        let dir = tempfile::tempdir().expect("tempdir");
        git_run(dir.path(), &["init", "-q", "-b", "main"]);
        git_run(dir.path(), &["commit", "-q", "--allow-empty", "-m", "base"]);
        std::fs::write(dir.path().join("scratch.log"), "noise").expect("write");

        resolve_base(dir.path().to_str().expect("utf-8"), false)
            .await
            .expect("an untracked file is not a reason to refuse");
    }
}
