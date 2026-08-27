use std::sync::Arc;

use axum::{extract::Extension, Json};
use serde::Deserialize;

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::work_task_batch as core;
use crate::models::{
    BatchCleanupOutcome, BatchMemberOutcome, WorkTaskBatchInfo, WorkTaskBatchSpec,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListParams {
    #[serde(default)]
    pub folder_id: Option<i32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdParams {
    pub id: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateParams {
    pub spec: WorkTaskBatchSpec,
}

/// Group tasks that already exist. `allowDirty` defaults, so a body without it
/// takes the safe path.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptParams {
    pub folder_id: i32,
    pub title: String,
    pub task_ids: Vec<i32>,
    #[serde(default)]
    pub allow_dirty: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupParams {
    pub id: i32,
    /// Members to keep. Absent/empty = clean every member.
    #[serde(default)]
    pub keep_task_ids: Vec<i32>,
}

pub async fn work_task_batch_list(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<ListParams>,
) -> Result<Json<Vec<WorkTaskBatchInfo>>, AppCommandError> {
    let result = core::work_task_batch_list_core(&state.db, params.folder_id)
        .await
        .map_err(AppCommandError::from)?;
    Ok(Json(result))
}

pub async fn work_task_batch_get(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<IdParams>,
) -> Result<Json<WorkTaskBatchInfo>, AppCommandError> {
    let result = core::work_task_batch_get_core(&state.db, params.id)
        .await
        .map_err(AppCommandError::from)?;
    Ok(Json(result))
}

pub async fn work_task_batch_create(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<CreateParams>,
) -> Result<Json<WorkTaskBatchInfo>, AppCommandError> {
    let result = core::work_task_batch_create_core(&state.db, &state.emitter, params.spec).await?;
    Ok(Json(result))
}

pub async fn work_task_batch_adopt(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AdoptParams>,
) -> Result<Json<WorkTaskBatchInfo>, AppCommandError> {
    let result = core::work_task_batch_adopt_core(
        &state.db,
        &state.emitter,
        params.folder_id,
        params.title,
        params.task_ids,
        params.allow_dirty,
    )
    .await?;
    Ok(Json(result))
}

/// Per-member outcomes, not a single verdict: a member that refuses to start
/// leaves the others running, and the caller has to be able to say which.
pub async fn work_task_batch_start(
    Json(params): Json<IdParams>,
) -> Result<Json<Vec<BatchMemberOutcome>>, AppCommandError> {
    let result = core::work_task_batch_start_core(params.id)
        .await
        .map_err(AppCommandError::from)?;
    Ok(Json(result))
}

pub async fn work_task_batch_cancel(
    Json(params): Json<IdParams>,
) -> Result<Json<Vec<BatchMemberOutcome>>, AppCommandError> {
    let result = core::work_task_batch_cancel_core(params.id)
        .await
        .map_err(AppCommandError::from)?;
    Ok(Json(result))
}

/// One `succeeded` / `failed` / `blocked` per attempted member, each already
/// persisted on its member row. Clients must render these individually — a
/// blanket "cleanup done" over this response is the false success the whole
/// shape exists to prevent.
pub async fn work_task_batch_cleanup(
    Json(params): Json<CleanupParams>,
) -> Result<Json<Vec<BatchCleanupOutcome>>, AppCommandError> {
    let result = core::work_task_batch_cleanup_core(params.id, params.keep_task_ids)
        .await
        .map_err(AppCommandError::from)?;
    Ok(Json(result))
}

pub async fn work_task_batch_delete(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<IdParams>,
) -> Result<Json<()>, AppCommandError> {
    core::work_task_batch_delete_core(&state.db, &state.emitter, params.id)
        .await
        .map_err(AppCommandError::from)?;
    Ok(Json(()))
}
