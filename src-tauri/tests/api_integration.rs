//! HTTP API integration tests.
//!
//! Builds the real Axum router from `web::router::build_router`, wired to an
//! in-memory SQLite database (`fresh_in_memory_db`) and a `WebOnly` event
//! emitter (`EventEmitter::test_web_only`). Drives requests through
//! `axum-test::TestServer` so no TCP socket is involved.
//!
//! Scope of this first pass:
//! - Authentication matrix on a representative protected endpoint
//! - Public endpoint (`get_system_language_settings`) reachable without token
//! - DB-backed endpoints (`load_folder_history`, `list_open_folders`) return
//!   the expected JSON shape.
//!
//! No test here may call `list_folders`, `get_stats`, `get_sidebar_data` or
//! `list_conversations`, not even just for a status code: they ignore the test
//! DB and parse every agent's session history under the real home directory.
//! That is instant on a clean CI runner and over ten minutes on a developer
//! machine with a few gigabytes of history.
//!
//! Not covered: WebSocket attach (separate concern), endpoints that touch the
//! Tauri webview (those are gated behind `tauri-runtime`).

use std::sync::Arc;

use axum_test::TestServer;
use codeg_lib::app_state::AppState;
use codeg_lib::db::test_helpers::fresh_in_memory_db;
use codeg_lib::web::router::build_router;
use codeg_lib::web::shutdown::ShutdownSignal;
use serde_json::{json, Value};

const TEST_TOKEN: &str = "integration-test-token";

async fn build_test_server() -> (TestServer, tempfile::TempDir, tempfile::TempDir) {
    let data_dir = tempfile::tempdir().expect("data dir");
    let static_dir = tempfile::tempdir().expect("static dir");

    let db = fresh_in_memory_db().await;
    let state = Arc::new(AppState::new_for_test(db, data_dir.path().to_path_buf()));
    let shutdown = Arc::new(ShutdownSignal::new());

    let router = build_router(
        state,
        TEST_TOKEN.to_string(),
        static_dir.path().to_path_buf(),
        shutdown,
    );

    let server = TestServer::new(router).expect("test server");
    // Keep data_dir and static_dir alive for the whole test by returning them.
    (server, data_dir, static_dir)
}

// ────────────────────────────────────────────────────────────────────────────
// Auth matrix
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn protected_endpoint_rejects_missing_token() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server.post("/api/list_open_folders").json(&json!({})).await;
    assert_eq!(resp.status_code(), 401);
}

#[tokio::test]
async fn protected_endpoint_rejects_wrong_token() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/list_open_folders")
        .add_header("authorization", "Bearer wrong-token")
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[tokio::test]
async fn protected_endpoint_accepts_correct_token() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/list_open_folders")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    // The handler's own answer from the fresh DB, not just a status code.
    assert_eq!(resp.json::<Value>(), json!([]));
}

// ────────────────────────────────────────────────────────────────────────────
// Public endpoint
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn public_language_settings_reachable_without_token() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/get_system_language_settings")
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    // Shape contract: returns a JSON object (exact fields vary by default).
    assert!(body.is_object(), "expected object body, got {body}");
}

// ────────────────────────────────────────────────────────────────────────────
// DB-backed endpoint
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn load_folder_history_returns_empty_array_on_fresh_db() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/load_folder_history")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    assert_eq!(
        body.as_array().expect("array body").len(),
        0,
        "fresh DB should have no folder history"
    );
}

#[tokio::test]
async fn open_folder_then_list_open_folders_shows_it() {
    let (server, _data, _static) = build_test_server().await;
    let open_resp = server
        .post("/api/open_folder")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({"path": "/tmp/codeg-test-folder"}))
        .await;
    assert_eq!(
        open_resp.status_code(),
        200,
        "open_folder failed: {}",
        open_resp.text()
    );

    let list_resp = server
        .post("/api/list_open_folders")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(list_resp.status_code(), 200);
    let body: Value = list_resp.json();
    let arr = body.as_array().expect("array");
    assert_eq!(
        arr.len(),
        1,
        "list_open_folders should reflect the open_folder call, got {body}"
    );
}

#[tokio::test]
async fn acp_find_connection_for_conversation_returns_null_when_none_live() {
    // No live ACP connection is bound to any conversation on a fresh server, so
    // discovery returns JSON `null` (Option::None) with 200 — the frontend
    // reads this as "no live owner, open the persisted detail instead".
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/acp_find_connection_for_conversation")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({"conversationId": 999, "agentType": "claude_code"}))
        .await;
    assert_eq!(resp.status_code(), 200, "body: {}", resp.text());
    let body: Value = resp.json();
    assert!(
        body.is_null(),
        "expected null for an unbound conversation, got {body}"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// Field naming sanity (snake_case ↔ camelCase boundary)
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn health_endpoint_returns_status_field() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/health")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn unknown_endpoint_returns_501_with_typed_error() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/this_endpoint_does_not_exist")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 501);
    let body: Value = resp.json();
    assert_eq!(body["code"], "not_implemented");
    assert!(body["message"].is_string());
}

// ────────────────────────────────────────────────────────────────────────────
// Live feedback settings + submit gate
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn feedback_settings_round_trip_defaults_off() {
    let (server, _data, _static) = build_test_server().await;
    // Default is OFF (opt-in feature).
    let resp = server
        .post("/api/get_feedback_settings")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    assert_eq!(resp.json::<Value>()["enabled"], false);

    // Enable it.
    let resp = server
        .post("/api/set_feedback_settings")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "settings": { "enabled": true } }))
        .await;
    assert_eq!(resp.status_code(), 200);
    assert_eq!(resp.json::<Value>()["enabled"], true);

    // Reads back enabled.
    let resp = server
        .post("/api/get_feedback_settings")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.json::<Value>()["enabled"], true);
}

// The submit gate is per-connection (the agent's actual `check_user_feedback`
// capability), unit-tested in `ConnectionManager::submit_feedback`
// (`submit_feedback_rejected_when_tool_unavailable`), not via the global setting.

// ────────────────────────────────────────────────────────────────────────────
// Response compression (allowlist predicate) + turn-window params
// ────────────────────────────────────────────────────────────────────────────

async fn build_test_server_with_state() -> (
    TestServer,
    Arc<AppState>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let data_dir = tempfile::tempdir().expect("data dir");
    let static_dir = tempfile::tempdir().expect("static dir");

    let db = fresh_in_memory_db().await;
    let state = Arc::new(AppState::new_for_test(db, data_dir.path().to_path_buf()));
    let shutdown = Arc::new(ShutdownSignal::new());

    let router = build_router(
        state.clone(),
        TEST_TOKEN.to_string(),
        static_dir.path().to_path_buf(),
        shutdown,
    );

    let server = TestServer::new(router).expect("test server");
    (server, state, data_dir, static_dir)
}

#[tokio::test]
async fn json_api_is_gzip_compressed_when_client_accepts() {
    let (server, state, _data, _static) = build_test_server_with_state().await;
    // Seed one folder so the JSON body clears the size-above threshold. The
    // endpoint has to be a DB-backed one: `/api/list_folders` ignores this
    // row and parses the *real* home directory instead, so it answers `[]`
    // — 2 bytes, under the threshold — on a clean CI runner while returning
    // a fat body on a developer machine.
    codeg_lib::db::service::folder_service::add_folder(
        &state.db.conn,
        "/tmp/codeg-compression-test-folder-with-a-reasonably-long-path",
    )
    .await
    .expect("seed folder");

    // Control: the same body, uncompressed, is over `MIN_COMPRESS_BYTES` —
    // that is what makes the gzip assertion below meaningful rather than an
    // accident of how much the host happens to have on disk.
    let plain = server
        .post("/api/list_open_folders")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(plain.status_code(), 200);
    assert_eq!(
        plain.json::<Value>().as_array().expect("array body").len(),
        1,
        "the seeded folder must be what this endpoint answers with"
    );
    assert!(
        plain.text().len() >= 32,
        "body must clear the size-above threshold, got {} bytes",
        plain.text().len()
    );

    let resp = server
        .post("/api/list_open_folders")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .add_header("accept-encoding", "gzip")
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    assert_eq!(
        resp.headers()
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("gzip"),
        "JSON API responses must be gzip-compressed when the client accepts it"
    );
}

#[tokio::test]
async fn static_js_is_compressed_but_binary_download_is_not() {
    let (server, _data, static_dir) = build_test_server().await;
    let big_text = "console.log('x');\n".repeat(200);
    std::fs::write(static_dir.path().join("chunk.js"), &big_text).expect("write js");
    std::fs::write(static_dir.path().join("blob.bin"), vec![0u8; 4096]).expect("write bin");

    let js = server
        .get("/chunk.js")
        .add_header("accept-encoding", "gzip")
        .await;
    assert_eq!(js.status_code(), 200);
    assert_eq!(
        js.headers()
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("gzip"),
        "static JS must compress"
    );

    let bin = server
        .get("/blob.bin")
        .add_header("accept-encoding", "gzip")
        .await;
    assert_eq!(bin.status_code(), 200);
    assert!(
        bin.headers().get("content-encoding").is_none(),
        "binary responses must stay identity-encoded (Content-Length preserved for progress)"
    );
    assert_eq!(
        bin.headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok()),
        Some("4096"),
        "binary Content-Length must survive the compression layer"
    );
}

fn cache_control(resp: &axum_test::TestResponse) -> Option<String> {
    resp.headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

#[tokio::test]
async fn static_files_revalidate_except_fingerprinted_assets() {
    let (server, _data, static_dir) = build_test_server().await;
    let root = static_dir.path();
    std::fs::write(root.join("index.html"), "<html>index</html>").expect("write index");
    std::fs::write(root.join("workspace.html"), "<html>workspace</html>")
        .expect("write workspace");
    std::fs::write(root.join("manifest.json"), "{}").expect("write manifest");
    std::fs::create_dir_all(root.join("_next/static/chunks")).expect("mkdir chunks");
    std::fs::write(root.join("_next/static/chunks/app-1a2b.js"), "0").expect("write chunk");

    // Same name in every release: a browser that kept one without asking
    // again would open the previous release after the server was upgraded.
    for path in ["/", "/workspace", "/manifest.json"] {
        let resp = server.get(path).await;
        assert_eq!(resp.status_code(), 200, "{path}");
        assert_eq!(cache_control(&resp).as_deref(), Some("no-cache"), "{path}");
    }

    // A fingerprinted asset never changes under its name.
    let chunk = server.get("/_next/static/chunks/app-1a2b.js").await;
    assert_eq!(chunk.status_code(), 200);
    assert_eq!(cache_control(&chunk), None);
    // The index.html standing in for a missing one is still a document.
    let missing = server.get("/_next/static/chunks/gone.js").await;
    assert_eq!(missing.text(), "<html>index</html>");
    assert_eq!(cache_control(&missing).as_deref(), Some("no-cache"));

    // Revalidating keeps the copy: it is not modified, so nothing is resent.
    let first = server.get("/").await;
    let last_modified = first
        .headers()
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .expect("last-modified")
        .to_owned();
    let again = server
        .get("/")
        .add_header("if-modified-since", last_modified)
        .await;
    assert_eq!(again.status_code(), 304);
    assert_eq!(cache_control(&again).as_deref(), Some("no-cache"));

    let api = server
        .post("/api/health")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(api.status_code(), 200);
    assert_eq!(cache_control(&api), None, "the API is not a static file");
}

#[tokio::test]
async fn get_folder_conversation_accepts_turn_window_params() {
    let (server, state, _data, _static) = build_test_server_with_state().await;
    let folder_id = codeg_lib::db::service::folder_service::add_folder(
        &state.db.conn,
        "/tmp/codeg-window-param-test",
    )
    .await
    .expect("seed folder")
    .id;
    let conv_id = codeg_lib::commands::conversations::create_conversation_core(
        &state.db.conn,
        folder_id,
        codeg_lib::models::AgentType::ClaudeCode,
        None,
    )
    .await
    .expect("create conversation");

    // tailTurns → windowed response: marker fields present even for an empty
    // transcript (offset/total 0, fingerprint = seed).
    let resp = server
        .post("/api/get_folder_conversation")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "conversationId": conv_id, "tailTurns": 50 }))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body = resp.json::<Value>();
    assert_eq!(body["turns_offset"], 0);
    assert_eq!(body["turns_total"], 0);
    assert_eq!(body["assistant_turns_before_offset"], 0);
    assert_eq!(body["user_turns_before_offset"], 0);
    assert!(body["prefix_hash"].is_string());
    let window_prefix_hash = body["prefix_hash"].clone();

    // No params → legacy full response: none of the window fields serialize.
    let resp = server
        .post("/api/get_folder_conversation")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "conversationId": conv_id }))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body = resp.json::<Value>();
    assert!(body.get("turns_offset").is_none());
    assert!(body.get("user_turns_before_offset").is_none());
    assert!(body.get("prefix_hash").is_none());

    // Both selectors → invalid input.
    let resp = server
        .post("/api/get_folder_conversation")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "conversationId": conv_id, "tailTurns": 5, "fromIndex": 3 }))
        .await;
    assert!(
        resp.status_code().is_client_error(),
        "tailTurns+fromIndex must be rejected, got {}",
        resp.status_code()
    );

    // Turns page endpoint responds with the seam fields.
    let resp = server
        .post("/api/get_folder_conversation_turns")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "conversationId": conv_id, "beforeIndex": 10, "limit": 5 }))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body = resp.json::<Value>();
    assert_eq!(body["turns_total"], 0);
    assert_eq!(body["user_turns_before_offset"], 0);
    assert!(body["prefix_hash"].is_string());
    assert!(body["prefix_hash_before_index"].is_string());

    // The navigator's outline of the history above the window: the prefix it
    // covers fingerprints like the window's own prefix hash.
    let resp = server
        .post("/api/get_folder_conversation_outline")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "conversationId": conv_id, "beforeIndex": 0 }))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body = resp.json::<Value>();
    assert_eq!(body["turns"], json!([]));
    assert_eq!(body["turns_total"], 0);
    assert_eq!(body["prefix_hash"], window_prefix_hash);
}

// ────────────────────────────────────────────────────────────────────────────
// codeg-mcp service status
// ────────────────────────────────────────────────────────────────────────────

/// The status endpoint has to answer even in a runtime that never bound a
/// broker socket — that IS the "not running" case the workspace indicator
/// exists to show, so a 500 here would blind exactly the situation it reports.
/// `AppState::new_for_test` installs no service handle, which is that runtime.
#[tokio::test]
async fn codeg_mcp_service_status_reports_a_socketless_runtime_as_stopped() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/get_codeg_mcp_service_status")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body = resp.json::<Value>();
    assert_eq!(body["state"], "stopped");
    assert_eq!(body["listening"], false);
    // No handle ⇒ nothing this process can start; the UI hides its button on
    // this flag rather than offering one that can only fail.
    assert_eq!(body["can_start"], false);
    // The switches ride along regardless of socket health, so the popover can
    // explain a healthy-but-toolless service without a second round trip.
    let groups = body["tool_groups"].as_array().expect("tool_groups array");
    let keys: Vec<&str> = groups.iter().filter_map(|g| g["key"].as_str()).collect();
    assert!(keys.contains(&"delegation"), "got {keys:?}");
    assert!(keys.contains(&"feedback"), "got {keys:?}");
}

/// Starting without a handle must fail loudly rather than report success the
/// UI would then paint as a running service.
#[tokio::test]
async fn starting_codeg_mcp_service_without_a_handle_is_rejected() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/start_codeg_mcp_service")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 422);
}

#[tokio::test]
async fn codeg_mcp_service_status_requires_a_token() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/get_codeg_mcp_service_status")
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 401);
}

// ────────────────────────────────────────────────────────────────────────────
// DeepSeek Harness model catalog
//
// Read-only side only: the update route writes the caller's real
// `$DSH_HOME/settings.yaml`, which a test must never do. The assertions stay
// content-agnostic for the same reason — the host may or may not have a
// harness settings document, and either is a valid answer here.
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn deepseek_model_catalog_is_readable_and_shaped_for_the_panel() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/acp_load_deepseek_model_catalog")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    // camelCase on the wire, and every field the panel branches on is present
    // — a missing document is reported in-band, never as an error status.
    assert!(body["path"].is_string(), "got {body}");
    assert!(body["exists"].is_boolean(), "got {body}");
    assert!(body["configured"].is_boolean(), "got {body}");
    assert!(body["models"].is_array(), "got {body}");
    assert!(body["error"].is_string() || body["error"].is_null(), "got {body}");
}

#[tokio::test]
async fn deepseek_model_catalog_requires_a_token() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/acp_load_deepseek_model_catalog")
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 401);
}

// ────────────────────────────────────────────────────────────────────────────
// Computer use
//
// Served only by a codeg-server its operator let share the screen it runs on
// (`CODEG_COMPUTER_USE`): everywhere else every call is refused, and
// `computer_available` says so up front. Nothing here reaches a helper — no
// screen is read in a test.
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn computer_use_is_refused_where_it_is_not_served() {
    let (server, _data, _static) = build_test_server().await;
    let auth = format!("Bearer {TEST_TOKEN}");
    let resp = server
        .post("/api/computer_available")
        .add_header("authorization", auth.clone())
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    assert_eq!(body["available"], false, "got {body}");
    assert!(body["platform"].is_string(), "got {body}");
    for (route, args) in [
        ("/api/computer_status", json!({})),
        ("/api/computer_shared_state", json!({})),
        (
            "/api/computer_share_window",
            json!({ "targetId": "w1", "level": "control" }),
        ),
        ("/api/computer_share_screen", json!({ "level": "read" })),
        ("/api/computer_stop", json!({})),
    ] {
        let resp = server
            .post(route)
            .add_header("authorization", auth.clone())
            .json(&args)
            .await;
        assert_eq!(resp.status_code(), 422, "{route}");
        let body: Value = resp.json();
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|m| m.contains("CODEG_COMPUTER_USE")),
            "{route}: {body}"
        );
    }
}

#[tokio::test]
async fn computer_use_requires_a_token() {
    let (server, _data, _static) = build_test_server().await;
    for route in ["/api/computer_available", "/api/computer_share_screen"] {
        let resp = server.post(route).json(&json!({ "level": "read" })).await;
        assert_eq!(resp.status_code(), 401, "{route}");
    }
}

/// A server let share its screen answers its web clients from its own
/// service — what is shared, and refusals of its own (here: computer use is
/// switched off, which a share needs first).
#[tokio::test]
async fn a_server_that_shares_its_screen_answers_its_web_clients() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let static_dir = tempfile::tempdir().expect("static dir");
    let db = fresh_in_memory_db().await;
    let state = Arc::new(AppState::new_for_test(db, data_dir.path().to_path_buf()));
    let service = codeg_lib::commands::computer::ComputerService::start(
        codeg_lib::commands::computer::ComputerHost::Server {
            broadcaster: state.event_broadcaster.clone(),
            emitter: state.emitter.clone(),
        },
        state.computer_tools_config.clone(),
    );
    assert!(state.computer_service.set(service).is_ok());
    assert!(state.computer_tools_config.is_served());
    let router = build_router(
        state,
        TEST_TOKEN.to_string(),
        static_dir.path().to_path_buf(),
        Arc::new(ShutdownSignal::new()),
    );
    let server = TestServer::new(router).expect("test server");
    let auth = format!("Bearer {TEST_TOKEN}");

    let resp = server
        .post("/api/computer_available")
        .add_header("authorization", auth.clone())
        .json(&json!({}))
        .await;
    assert_eq!(resp.json::<Value>()["available"], true);

    let resp = server
        .post("/api/computer_shared_state")
        .add_header("authorization", auth.clone())
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    assert_eq!(resp.json::<Value>(), json!({ "shared": [], "apps": [] }));

    let resp = server
        .post("/api/computer_share_screen")
        .add_header("authorization", auth.clone())
        .json(&json!({ "level": "read" }))
        .await;
    assert_eq!(resp.status_code(), 422);
    let body: Value = resp.json();
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|m| m.contains("switched off") || m.contains("not offered on Linux")),
        "got {body}"
    );

    let resp = server
        .post("/api/computer_stop_key_status")
        .add_header("authorization", auth)
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    assert_eq!(resp.json::<Value>(), json!({}));
}

// ────────────────────────────────────────────────────────────────────────────
// Assistants
// ────────────────────────────────────────────────────────────────────────────

fn assistant_draft(name: &str) -> Value {
    json!({
        "name": name,
        "description": "Reviews diffs",
        "avatar": { "kind": "monogram", "color": "violet" },
        "agent_config": {
            "agent_type": "codex",
            "agent_options": { "config_values": { "model": "gpt-5.5" } },
            "label_snapshot": {
                "agent": "Codex CLI",
                "mode": null,
                "options": [
                    { "id": "model", "kind": "select", "name": "Model", "value_label": "GPT-5.5" }
                ]
            }
        },
        "instructions": "Be terse.",
        "enabled": true
    })
}

#[tokio::test]
async fn assistants_round_trip_over_http() {
    let (server, _data, _static) = build_test_server().await;
    let auth = format!("Bearer {TEST_TOKEN}");
    let call = |path: &'static str, body: Value| {
        server
            .post(path)
            .add_header("authorization", auth.clone())
            .json(&body)
    };

    let resp = call("/api/assistant_list", json!({})).await;
    assert_eq!(resp.status_code(), 200);
    assert_eq!(resp.json::<Value>(), json!([]));

    let resp = call("/api/assistant_create", json!({ "draft": assistant_draft("Reviewer") })).await;
    assert_eq!(resp.status_code(), 200);
    let a: Value = resp.json();
    assert_eq!(a["name"], "Reviewer");
    assert_eq!(a["revision"], 1);
    assert_eq!(a["agent_type"], "codex");
    assert!(a["deleted_at"].is_null());
    let a_id = a["id"].as_i64().unwrap();

    let resp = call("/api/assistant_create", json!({ "draft": assistant_draft("Writer") })).await;
    let b_id = resp.json::<Value>()["id"].as_i64().unwrap();

    // A duplicate name is a conflict carrying its i18n key.
    let resp = call("/api/assistant_create", json!({ "draft": assistant_draft("writer") })).await;
    assert_eq!(resp.status_code(), 409);
    assert_eq!(resp.json::<Value>()["i18n_key"], "errors.duplicateName");

    // An incomplete selection never lands.
    let mut incomplete = assistant_draft("Other");
    incomplete["agent_config"]["agent_options"]["config_values"] = json!({});
    let resp = call("/api/assistant_create", json!({ "draft": incomplete })).await;
    assert_eq!(resp.status_code(), 400);
    assert_eq!(resp.json::<Value>()["i18n_key"], "errors.invalidAgentOptions");

    let resp = call(
        "/api/assistant_update",
        json!({ "id": a_id, "patch": { "enabled": false } }),
    )
    .await;
    assert_eq!(resp.status_code(), 200);
    let updated: Value = resp.json();
    assert_eq!(updated["enabled"], false);
    assert_eq!(updated["revision"], 2);
    assert_eq!(updated["instructions"], "Be terse.");

    let resp = call("/api/assistant_reorder", json!({ "ids": [b_id, a_id] })).await;
    assert_eq!(resp.status_code(), 200);
    let moved: Value = resp.json();
    assert_eq!(moved.as_array().unwrap().len(), 2);
    assert_eq!(moved[0]["id"], b_id);

    let resp = call("/api/assistant_delete", json!({ "id": a_id })).await;
    assert_eq!(resp.status_code(), 200);
    assert!(resp.json::<Value>()["deleted_at"].is_string());

    let resp = call("/api/assistant_delete", json!({ "id": a_id })).await;
    assert_eq!(resp.status_code(), 404);

    // The list keeps the tombstone, in display order.
    let resp = call("/api/assistant_list", json!({})).await;
    let list: Value = resp.json();
    let ids: Vec<i64> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![b_id, a_id]);
    assert!(list[1]["deleted_at"].is_string());
}

async fn build_test_server_with_db() -> (
    TestServer,
    tempfile::TempDir,
    tempfile::TempDir,
    sea_orm::DatabaseConnection,
) {
    let (server, state, data, static_dir) = build_test_server_with_state().await;
    (server, data, static_dir, state.db.conn.clone())
}

#[tokio::test]
async fn delegation_performance_reports_ledger_rows_over_http() {
    use codeg_lib::acp::delegation::types::{DelegationTaskReport, TaskStatus};
    use codeg_lib::db::service::{conversation_service, delegation_task_service, folder_service};
    use codeg_lib::models::AgentType;

    let (server, _data, _static, conn) = build_test_server_with_db().await;

    let folder = folder_service::add_folder(&conn, "/workspace/delegation")
        .await
        .expect("folder");
    let parent = conversation_service::create(&conn, folder.id, AgentType::ClaudeCode, None, None)
        .await
        .expect("parent");
    let child = conversation_service::create(&conn, folder.id, AgentType::Codex, None, None)
        .await
        .expect("child");
    let binding = delegation_task_service::ResumeBinding {
        agent_type: AgentType::Codex,
        external_session_id: "session-1".into(),
        child_conversation_id: child.id,
        working_dir: "/workspace/delegation".into(),
        preferred_mode_id: None,
        preferred_config_values: Default::default(),
        config_fingerprint: "fingerprint-1".into(),
    };
    delegation_task_service::admit(
        &conn,
        delegation_task_service::AdmissionInput {
            task_id: "t0".into(),
            parent_conversation_id: parent.id,
            child_conversation_id: child.id,
            source_task_id: None,
            task: "dashboard task".into(),
            requested_working_dir: None,
            resume_binding: binding,
            work_task_id: None,
        },
    )
    .await
    .expect("admit");
    // The terminal report carries no usage; the dashboard's numbers come from
    // the child's materialized token facts, summed by the freeze path.
    codeg_lib::db::service::token_usage_service::replace_conversation_facts(
        &conn,
        child.id,
        chrono::Utc::now(),
        &[codeg_lib::db::service::token_usage_service::UsageFact {
            turn_key: "turn-1".into(),
            occurred_at: chrono::Utc::now(),
            model: Some("gpt-6-astra".into()),
            input_tokens: 1_000,
            output_tokens: 250,
            cache_creation_tokens: 300,
            cache_read_tokens: 400,
            duration_ms: 10,
        }],
    )
    .await
    .expect("seed usage facts");
    let report = DelegationTaskReport {
        assistant_id: None,
        assistant_name: None,
        progress: None,
        task_id: Some("t0".into()),
        status: TaskStatus::Completed,
        child_conversation_id: Some(child.id),
        agent_type: Some(AgentType::Codex),
        text: Some("done".into()),
        error_code: None,
        message: None,
        duration_ms: Some(1_234),
        turn_count: Some(2),
        token_usage: None,
        blocked_on: None,
        // Default-model call: nothing requested, but the admission snapshot
        // still names what ran.
        selectors: Some(codeg_lib::acp::delegation::types::DelegationSelectorReport {
            requested: Default::default(),
            effective: Some(codeg_lib::acp::delegation::types::AppliedSelectors {
                mode: Some("default".into()),
                config_values: std::collections::BTreeMap::from([(
                    "model".to_string(),
                    "gpt-6-astra".to_string(),
                )]),
            }),
        }),
    };
    assert!(
        delegation_task_service::finish(&conn, parent.id, "t0", &report)
            .await
            .expect("finish")
    );

    let resp = server
        .post("/api/get_delegation_performance")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    assert_eq!(body["totals"]["task_count"], 1, "got {body}");
    assert_eq!(body["totals"]["completed"], 1);
    assert_eq!(body["totals"]["success_rate"], 1.0);
    assert_eq!(body["totals"]["rework_rate"], 0.0);
    assert_eq!(body["totals"]["avg_duration_ms"], 1_234.0);
    // Tokens come from the child's usage facts (report carried none), cache
    // split included.
    assert_eq!(body["totals"]["input_tokens"], 1_000);
    assert_eq!(body["totals"]["output_tokens"], 250);
    assert_eq!(body["totals"]["cache_creation_tokens"], 300);
    assert_eq!(body["totals"]["cache_read_tokens"], 400);
    assert_eq!(body["by_agent"][0]["key"], "codex");
    assert_eq!(body["by_agent"][0]["task_count"], 1);
    // The default-model call recorded the effective model from its snapshot.
    assert_eq!(body["by_model"][0]["key"], "gpt-6-astra");
    assert_eq!(body["by_model"][0]["task_count"], 1);
}

/// No usage facts is UNKNOWN, not zero: the dashboard keeps its "not recorded"
/// bucket and a zero total rather than inventing spend.
#[tokio::test]
async fn delegation_performance_without_usage_stays_unknown() {
    use codeg_lib::acp::delegation::types::{DelegationTaskReport, TaskStatus};
    use codeg_lib::db::service::{conversation_service, delegation_task_service, folder_service};
    use codeg_lib::models::AgentType;

    let (server, _data, _static, conn) = build_test_server_with_db().await;
    let folder = folder_service::add_folder(&conn, "/workspace/no-usage")
        .await
        .expect("folder");
    let parent = conversation_service::create(&conn, folder.id, AgentType::ClaudeCode, None, None)
        .await
        .expect("parent");
    let child = conversation_service::create(&conn, folder.id, AgentType::Codex, None, None)
        .await
        .expect("child");
    delegation_task_service::admit(
        &conn,
        delegation_task_service::AdmissionInput {
            task_id: "n0".into(),
            parent_conversation_id: parent.id,
            child_conversation_id: child.id,
            source_task_id: None,
            task: "no usage".into(),
            requested_working_dir: None,
            resume_binding: delegation_task_service::ResumeBinding {
                agent_type: AgentType::Codex,
                external_session_id: "session-n".into(),
                child_conversation_id: child.id,
                working_dir: "/workspace/no-usage".into(),
                preferred_mode_id: None,
                preferred_config_values: Default::default(),
                config_fingerprint: "fingerprint-n".into(),
            },
            work_task_id: None,
        },
    )
    .await
    .expect("admit");
    let report = DelegationTaskReport {
        assistant_id: None,
        assistant_name: None,
        progress: None,
        task_id: Some("n0".into()),
        status: TaskStatus::Completed,
        child_conversation_id: Some(child.id),
        agent_type: Some(AgentType::Codex),
        text: Some("done".into()),
        error_code: None,
        message: None,
        duration_ms: Some(10),
        turn_count: Some(1),
        token_usage: None,
        blocked_on: None,
        selectors: None,
    };
    assert!(
        delegation_task_service::finish(&conn, parent.id, "n0", &report)
            .await
            .expect("finish")
    );

    let resp = server
        .post("/api/get_delegation_performance")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    assert_eq!(body["totals"]["input_tokens"], 0);
    assert_eq!(body["totals"]["output_tokens"], 0);
    assert_eq!(body["totals"]["cache_creation_tokens"], 0);
    assert_eq!(body["totals"]["cache_read_tokens"], 0);
    assert_eq!(body["by_model"][0]["key"], Value::Null);
}

#[tokio::test]
async fn delegation_performance_on_empty_ledger_returns_zeroed_totals() {
    let (server, _data, _static, _conn) = build_test_server_with_db().await;
    let resp = server
        .post("/api/get_delegation_performance")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    assert_eq!(body["totals"]["task_count"], 0, "got {body}");
    assert_eq!(body["by_agent"].as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn delegation_performance_requires_a_token() {
    let (server, _data, _static, _conn) = build_test_server_with_db().await;
    let resp = server
        .post("/api/get_delegation_performance")
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[tokio::test]
async fn work_task_runs_and_delegations_report_over_http() {
    use codeg_lib::db::service::{
        conversation_service, folder_service, work_task_run_service, work_task_service,
    };
    use codeg_lib::models::{AgentType, WorkTaskDraft};

    let (server, _data, _static, conn) = build_test_server_with_db().await;

    let folder = folder_service::add_folder(&conn, "/workspace/run-ledger")
        .await
        .expect("folder");
    let conversation =
        conversation_service::create(&conn, folder.id, AgentType::ClaudeCode, None, None)
            .await
            .expect("conversation");
    let task = work_task_service::create(
        &conn,
        WorkTaskDraft {
            folder_id: folder.id,
            title: "ledger over http".to_string(),
            config: json!({ "display_text": "ledger over http" }),
        },
    )
    .await
    .expect("task");
    assert!(work_task_run_service::open(
        &conn,
        work_task_run_service::RunOpen {
            task_id: task.id,
            run_seq: 0,
            kind: work_task_run_service::KIND_FRESH,
            resume_outcome: Some(work_task_run_service::RESUME_FRESH_NO_SESSION),
            resumed_from_run_seq: None,
            agent_type: Some("claude_code"),
            conversation_id: Some(conversation.id),
            external_session_id: Some("session-http"),
            working_dir: Some("/tmp/wt"),
            effective_mode: Some("default"),
            effective_model: Some("claude-sonnet"),
            effective_reasoning_level: None,
        },
    )
    .await
    .expect("open run"));
    assert!(work_task_run_service::close(
        &conn,
        task.id,
        0,
        work_task_run_service::STATUS_SETTLED,
        None,
        Some("success"),
        None,
    )
    .await
    .expect("close run"));

    let resp = server
        .post("/api/work_task_runs")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "taskId": task.id }))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    assert_eq!(body.as_array().map(Vec::len), Some(1), "got {body}");
    assert_eq!(body[0]["run_seq"], 0);
    assert_eq!(body[0]["kind"], "fresh");
    assert_eq!(body[0]["status"], "settled");
    assert_eq!(body[0]["resume_outcome"], "fresh_no_session");
    assert_eq!(body[0]["external_session_id"], "session-http");
    assert_eq!(body[0]["verdict"], "success");

    // Another task's ledger is empty.
    let resp = server
        .post("/api/task_delegations")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "taskId": task.id }))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    assert_eq!(body.as_array().map(Vec::len), Some(0), "got {body}");
}

#[tokio::test]
async fn task_delegations_reports_the_attributed_ledger_rows() {
    use codeg_lib::db::service::{conversation_service, delegation_task_service, folder_service};
    use codeg_lib::models::AgentType;

    let (server, _data, _static, conn) = build_test_server_with_db().await;

    let folder = folder_service::add_folder(&conn, "/workspace/task-delegations")
        .await
        .expect("folder");
    let parent = conversation_service::create(&conn, folder.id, AgentType::ClaudeCode, None, None)
        .await
        .expect("parent");
    let child = conversation_service::create(&conn, folder.id, AgentType::Codex, None, None)
        .await
        .expect("child");
    delegation_task_service::admit(
        &conn,
        delegation_task_service::AdmissionInput {
            task_id: "linked".into(),
            parent_conversation_id: parent.id,
            child_conversation_id: child.id,
            source_task_id: None,
            task: "linked work".into(),
            requested_working_dir: None,
            resume_binding: delegation_task_service::ResumeBinding {
                agent_type: AgentType::Codex,
                external_session_id: "session-linked".into(),
                child_conversation_id: child.id,
                working_dir: "/workspace/task-delegations".into(),
                preferred_mode_id: None,
                preferred_config_values: Default::default(),
                config_fingerprint: "fp".into(),
            },
            work_task_id: Some(77),
        },
    )
    .await
    .expect("admit");

    let resp = server
        .post("/api/task_delegations")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "taskId": 77 }))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    assert_eq!(body.as_array().map(Vec::len), Some(1), "got {body}");
    assert_eq!(body[0]["task_id"], "linked");
    assert_eq!(body[0]["task"], "linked work");
    assert_eq!(body[0]["status"], "running");
    assert_eq!(body[0]["child_conversation_id"], child.id);
    assert_eq!(body[0]["agent_type"], "codex");

    // An unattributed task sees nothing.
    let resp = server
        .post("/api/task_delegations")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "taskId": 78 }))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    assert_eq!(body.as_array().map(Vec::len), Some(0), "got {body}");
}

#[tokio::test]
async fn work_task_ledger_reads_require_a_token() {
    let (server, _data, _static, _conn) = build_test_server_with_db().await;
    for route in ["/api/work_task_runs", "/api/task_delegations"] {
        let resp = server.post(route).json(&json!({ "taskId": 1 })).await;
        assert_eq!(resp.status_code(), 401, "{route}");
    }
}

#[tokio::test]
async fn work_task_blocked_state_and_dependency_removal_over_http() {
    use codeg_lib::db::service::folder_service;
    use codeg_lib::db::service::work_task_service::{self, WorkTaskChildDraft};
    use codeg_lib::models::WorkTaskDraft;

    let (server, _data, _static, conn) = build_test_server_with_db().await;
    let folder = folder_service::add_folder(&conn, "/tmp/api-orchestration")
        .await
        .expect("folder");
    let draft = |title: &str| WorkTaskDraft {
        folder_id: folder.id,
        title: title.to_string(),
        config: json!({
            "display_text": "do it",
            "prompt_blocks": [{ "type": "text", "text": "do it" }],
        }),
    };
    let parent = work_task_service::create(&conn, draft("integrate"))
        .await
        .expect("parent");
    // The parent waits for its child, so it is blocked by the gate.
    work_task_service::split_task(
        &conn,
        work_task_service::SplitTaskRequest {
            parent_id: parent.id,
            children: vec![WorkTaskChildDraft {
                title: "piece".to_string(),
                config: json!({
                    "display_text": "do the piece",
                    "prompt_blocks": [{ "type": "text", "text": "do the piece" }],
                }),
                depends_on_index: vec![],
            }],
            depends_on_task_ids: vec![],
            max_concurrent_children: None,
            max_runs_per_child: None,
            token_budget: None,
            parent_depends_on_children: true,
            created_by_conversation_id: None,
        },
    )
    .await
    .expect("split");
    let child = work_task_service::children_of(&conn, parent.id)
        .await
        .expect("children")[0]
        .id;

    let resp = server
        .post("/api/work_task_get")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "id": parent.id }))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    assert_eq!(body["blocked"]["reason"], "dependency", "got {body}");
    assert_eq!(body["blocked"]["dependencies"][0]["task_id"], child);

    // Removing the edge unblocks it — and the second call reports "nothing to
    // remove" rather than failing.
    let resp = server
        .post("/api/work_task_dependency_remove")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "taskId": parent.id, "dependsOnTaskId": child }))
        .await;
    assert_eq!(resp.status_code(), 200);
    assert_eq!(resp.json::<Value>(), Value::Bool(true));

    let resp = server
        .post("/api/work_task_get")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "id": parent.id }))
        .await;
    let body: Value = resp.json();
    assert!(body.get("blocked").is_none(), "got {body}");

    let resp = server
        .post("/api/work_task_dependency_remove")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "taskId": parent.id, "dependsOnTaskId": child }))
        .await;
    assert_eq!(resp.json::<Value>(), Value::Bool(false));

    // The route is protected like every other one.
    let resp = server
        .post("/api/work_task_dependency_remove")
        .json(&json!({ "taskId": parent.id, "dependsOnTaskId": child }))
        .await;
    assert_eq!(resp.status_code(), 401);
}
