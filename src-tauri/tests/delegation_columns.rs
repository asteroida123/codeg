//! Verifies the m20260522 migration added `parent_tool_use_id` and
//! `delegation_call_id` columns on `conversation`, and they round-trip via the
//! SeaORM entity.

use codeg_lib::db::entities::conversation;
use codeg_lib::db::test_helpers::{fresh_in_memory_db, seed_folder};
use codeg_lib::models::agent::AgentType;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, NotSet, QueryFilter, Set};

#[tokio::test]
async fn delegation_columns_round_trip() {
    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, "/tmp/codeg-delegation-test").await;

    let agent_type_str = serde_json::to_value(AgentType::ClaudeCode)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let now = chrono::Utc::now();
    let active = conversation::ActiveModel {
        id: NotSet,
        folder_id: Set(folder_id),
        title: Set(Some("delegation child".to_string())),
        title_locked: Set(false),
        agent_type: Set(agent_type_str),
        status: Set(conversation::ConversationStatus::InProgress),
        kind: Set(conversation::ConversationKind::Delegate),
        model: Set(None),
        git_branch: Set(None),
        external_id: Set(None),
        parent_id: Set(Some(42)),
        parent_tool_use_id: Set(Some("toolu_abc123".to_string())),
        delegation_call_id: Set(Some("00000000-0000-0000-0000-000000000001".to_string())),
        message_count: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
        pinned_at: Set(None),
        origin_cwd: Set(None),
    };
    let inserted = active.insert(&db.conn).await.expect("insert");
    let id = inserted.id;

    let fetched = conversation::Entity::find_by_id(id)
        .one(&db.conn)
        .await
        .expect("query ok")
        .expect("row exists");
    assert_eq!(fetched.parent_id, Some(42));
    assert_eq!(fetched.parent_tool_use_id.as_deref(), Some("toolu_abc123"));
    assert_eq!(
        fetched.delegation_call_id.as_deref(),
        Some("00000000-0000-0000-0000-000000000001")
    );
}

#[tokio::test]
async fn delegation_columns_default_to_null_on_existing_create() {
    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, "/tmp/codeg-delegation-null").await;
    // The existing create helper does not set the new columns; verify they default to None.
    let conv_id =
        codeg_lib::db::test_helpers::seed_conversation(&db, folder_id, AgentType::ClaudeCode).await;
    let fetched = conversation::Entity::find_by_id(conv_id)
        .one(&db.conn)
        .await
        .expect("query ok")
        .expect("row exists");
    assert_eq!(fetched.parent_id, None);
    assert_eq!(fetched.parent_tool_use_id, None);
    assert_eq!(fetched.delegation_call_id, None);
}

// ── work_task attribution on the delegation ledger (phase 2, #731) ──────────

/// The `delegation_task.work_task_id` column (migration
/// `m20260926_000004_delegation_work_task_link`) round-trips through the
/// entity AND through the admission service, which is what actually writes it:
/// a delegation admitted while a work task executed must carry that task, and
/// a plain chat delegation must not.
#[tokio::test]
async fn delegation_work_task_attribution_round_trips() {
    use codeg_lib::db::entities::delegation_task;
    use codeg_lib::db::service::delegation_task_service::{
        admit, AdmissionInput, AdmissionResult, ResumeBinding,
    };
    use std::collections::BTreeMap;

    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, "/tmp/codeg-work-task-link").await;
    let parent =
        codeg_lib::db::test_helpers::seed_conversation(&db, folder_id, AgentType::ClaudeCode).await;
    let child =
        codeg_lib::db::test_helpers::seed_conversation(&db, folder_id, AgentType::Codex).await;

    let binding = ResumeBinding {
        agent_type: AgentType::Codex,
        external_session_id: "session-link".into(),
        child_conversation_id: child,
        working_dir: "/tmp/codeg-work-task-link".into(),
        preferred_mode_id: None,
        preferred_config_values: BTreeMap::new(),
        config_fingerprint: "fp-link".into(),
    };
    let result = admit(
        &db.conn,
        AdmissionInput {
            task_id: "linked-task".into(),
            parent_conversation_id: parent,
            child_conversation_id: child,
            source_task_id: None,
            task: "work".into(),
            requested_working_dir: None,
            resume_binding: binding,
            work_task_id: Some(42),
        },
    )
    .await
    .expect("admit");
    assert!(matches!(result, AdmissionResult::New { .. }));

    let row = delegation_task::Entity::find()
        .filter(delegation_task::Column::TaskId.eq("linked-task"))
        .one(&db.conn)
        .await
        .expect("query")
        .expect("row");
    assert_eq!(row.work_task_id, Some(42));

    // And the task-scoped read projects exactly the rows it stored.
    let rows = codeg_lib::db::service::delegation_task_service::list_for_task(&db.conn, 42)
        .await
        .expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].task_id, "linked-task");
    assert_eq!(rows[0].status, "running");
    assert_eq!(rows[0].child_conversation_id, child);
}
