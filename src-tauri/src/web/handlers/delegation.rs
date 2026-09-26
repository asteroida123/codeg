//! HTTP handlers for delegation settings — the web-mode mirror of the
//! Tauri commands in `commands::delegation`.
//!
//! Both endpoints share the same core helpers (`load_delegation_settings`,
//! `set_delegation_settings_core`) so the clamp + persist + broker
//! re-apply behavior stays identical across transports.

use std::sync::Arc;

use axum::{extract::Extension, Json};
use serde::Deserialize;

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::delegation::{
    get_delegation_performance_core, load_delegation_settings, set_delegation_settings_core,
    DelegationSettings,
};
use crate::db::service::delegation_task_service::DelegationPerformanceReport;

pub async fn get_delegation_settings(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<DelegationSettings>, AppCommandError> {
    Ok(Json(load_delegation_settings(&state.db.conn).await))
}

/// Sub-agent performance dashboard (upstream #724) — web-mode mirror of the
/// `get_delegation_performance` Tauri command.
pub async fn get_delegation_performance(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<DelegationPerformanceReport>, AppCommandError> {
    Ok(Json(get_delegation_performance_core(&state.db.conn).await?))
}

#[derive(Deserialize)]
pub struct SetDelegationSettingsParams {
    pub settings: DelegationSettings,
}

pub async fn set_delegation_settings(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<SetDelegationSettingsParams>,
) -> Result<Json<DelegationSettings>, AppCommandError> {
    let saved = set_delegation_settings_core(
        &state.db.conn,
        &state.delegation_broker,
        &state.emitter,
        params.settings,
    )
    .await?;
    Ok(Json(saved))
}
