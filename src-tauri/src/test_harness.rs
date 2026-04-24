//! Integration-test harness. Lets a test binary spin up N headless
//! "clients", each backed by a real [`tauri::App`] running on the
//! [`MockRuntime`], sharing a single test Turso database but isolated by
//! per-client [`InMemoryKeystore`]s.
//!
//! Gated on `feature = "test-harness"` so production builds don't pay for
//! this code.
//!
//! See `tests/flows.rs` for the orchestration layer (`TestWorld`,
//! `TestClient`) and scenario tests.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use tauri::test::{mock_builder, mock_context, noop_assets, MockRuntime, INVOKE_KEY};
use tauri::{App, WebviewWindow, WebviewWindowBuilder};

use crate::error::{Error, Result};
use crate::state::AppState;

/// Build a headless tauri app that has the full production command surface
/// registered and the given `AppState` managed. Each client in a multi-client
/// test owns a distinct `App<MockRuntime>` and therefore a distinct `AppState`
/// (distinct keystore, distinct `device_id`, distinct local DB handle), while
/// sharing the same underlying `Arc<RemoteDb>` against test Turso.
///
/// The returned `App` is NOT `run()` — tests drive commands directly via
/// [`invoke`] / [`invoke_unit`].
pub fn build_client_app(state: Arc<AppState>) -> Result<(App<MockRuntime>, WebviewWindow<MockRuntime>)> {
    let app = mock_builder()
        .invoke_handler(tauri::generate_handler![
            crate::commands::auth::initialize_identity,
            crate::commands::auth::get_identity,
            crate::commands::auth::request_otp,
            crate::commands::auth::verify_otp,
            crate::commands::auth::dev_login,
            crate::commands::auth::get_session,
            crate::commands::auth::logout,
            crate::commands::auth::delete_account,
            crate::commands::auth::list_known_accounts,
            crate::commands::auth::wipe_local_data,
            crate::commands::auth::list_user_devices,
            crate::commands::pin::set_pin,
            crate::commands::pin::unlock,
            crate::commands::pin::lock,
            crate::commands::pin::get_unlock_state,
            crate::commands::device_enrollment::start_device_enrollment,
            crate::commands::device_enrollment::poll_enrollment_status,
            crate::commands::device_enrollment::list_pending_enrollment_requests,
            crate::commands::device_enrollment::approve_device_enrollment,
            crate::commands::device_enrollment::reject_device_enrollment,
            crate::commands::device_enrollment::recover_with_secret_key,
            crate::commands::device_enrollment::reset_identity_and_recover,
            crate::commands::device_enrollment::list_security_events,
            crate::commands::user::get_user_profile,
            crate::commands::user::update_user_profile,
            crate::commands::user::search_user_by_username,
            crate::commands::user::get_preferences,
            crate::commands::user::save_preferences,
            crate::commands::groups::list_user_groups,
            crate::commands::groups::list_user_groups_with_channels,
            crate::commands::groups::list_group_channels,
            crate::commands::groups::create_group,
            crate::commands::groups::create_channel,
            crate::commands::groups::send_group_invite,
            crate::commands::groups::get_pending_invites,
            crate::commands::groups::accept_group_invite,
            crate::commands::groups::decline_group_invite,
            crate::commands::groups::request_group_access,
            crate::commands::groups::get_group_join_requests,
            crate::commands::groups::get_my_join_request,
            crate::commands::groups::approve_join_request,
            crate::commands::groups::reject_join_request,
            crate::commands::groups::update_group,
            crate::commands::groups::delete_group,
            crate::commands::groups::get_group_members,
            crate::commands::groups::remove_member_from_group,
            crate::commands::groups::leave_group,
            crate::commands::groups::update_channel,
            crate::commands::groups::delete_channel,
            crate::commands::groups::set_member_role,
            crate::commands::groups::search_group_by_slug,
            crate::commands::dm::create_dm_channel,
            crate::commands::dm::list_dm_channels,
            crate::commands::dm::list_dm_requests,
            crate::commands::dm::accept_dm_request,
            crate::commands::dm::get_dm_channel,
            crate::commands::dm::add_user_to_dm_channel,
            crate::commands::dm::remove_user_from_dm_channel,
            crate::commands::dm::leave_dm_channel,
            crate::commands::blocks::block_user,
            crate::commands::blocks::unblock_user,
            crate::commands::blocks::list_blocked_users,
            crate::commands::messages::list_messages,
            crate::commands::messages::send_message,
            crate::commands::messages::get_channel_messages,
            crate::commands::messages::get_dm_messages,
            crate::commands::messages::list_messages_by_sender,
            crate::commands::messages::list_channel_previews,
            crate::commands::messages::search_messages,
            crate::commands::messages::add_reaction,
            crate::commands::messages::remove_reaction,
            crate::commands::messages::get_reactions,
            crate::commands::messages::delete_message,
            crate::commands::messages::edit_message,
            crate::commands::mls::generate_mls_key_package,
            crate::commands::mls::publish_mls_key_package,
            crate::commands::mls::fetch_mls_key_package,
            crate::commands::mls::create_mls_group,
            crate::commands::mls::process_welcome,
            crate::commands::mls::poll_mls_welcomes,
            crate::commands::mls::reconcile_group_mls,
            crate::commands::mls::process_pending_commits,
        ])
        .manage(state)
        .build(mock_context(noop_assets()))
        .map_err(|e| Error::Other(anyhow::anyhow!("build mock app: {e}")))?;

    let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .map_err(|e| Error::Other(anyhow::anyhow!("build webview: {e}")))?;

    Ok((app, webview))
}

/// Dispatch an IPC message through the webview's command pipeline and
/// deserialize the result. Runs on a blocking thread because
/// [`tauri::test::get_ipc_response`] uses an `std::sync::mpsc` channel —
/// inside a tokio runtime we hand it off to `spawn_blocking` so it can't
/// starve the reactor.
pub async fn invoke<T>(
    webview: &WebviewWindow<MockRuntime>,
    cmd: &str,
    args: serde_json::Value,
) -> std::result::Result<T, String>
where
    T: DeserializeOwned + Send + 'static,
{
    let webview = webview.clone();
    let cmd = cmd.to_string();
    tokio::task::spawn_blocking(move || {
        let request = tauri::webview::InvokeRequest {
            cmd,
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: "http://tauri.localhost".parse().unwrap(),
            body: tauri::ipc::InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        };
        match tauri::test::get_ipc_response(&webview, request) {
            Ok(body) => body
                .deserialize::<T>()
                .map_err(|e| format!("deserialize response: {e}")),
            Err(v) => Err(v
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| v.to_string())),
        }
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

/// Invoke a command that returns `()` (or a `Result<(), _>`).
pub async fn invoke_unit(
    webview: &WebviewWindow<MockRuntime>,
    cmd: &str,
    args: serde_json::Value,
) -> std::result::Result<(), String> {
    // Unit commands return `null` in the JSON pipeline; deserialize through
    // `serde_json::Value` first and ignore the value.
    let _: serde_json::Value = invoke(webview, cmd, args).await?;
    Ok(())
}

// ── Schema bootstrap ────────────────────────────────────────────────────────
//
// The test Turso instance starts empty. Apply the canonical baseline (which
// captures the full current schema) on first run, and stamp
// `schema_migrations` so the DB looks adopted.

const BASELINE: &str = include_str!("db/migrations/000000_baseline.sql");

/// Apply the baseline schema to the shared test DB if it hasn't been applied
/// yet. Idempotent: safe to call on every test run.
pub async fn bootstrap_schema(remote: &crate::db::remote::RemoteDb) -> Result<()> {
    let conn = remote.conn().await?;

    // `users` is created by the baseline and never dropped — use it as a
    // marker for "baseline already applied."
    let has_baseline = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='users'",
            (),
        )
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("probe sqlite_master: {e}")))?
        .next()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("probe sqlite_master row: {e}")))?
        .is_some();

    if !has_baseline {
        run_sql_script(&conn, BASELINE, "baseline").await?;
    }

    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version     INTEGER PRIMARY KEY,
             description TEXT NOT NULL,
             applied_at  TEXT NOT NULL DEFAULT (datetime('now'))
         )",
        (),
    )
    .await
    .map_err(|e| Error::Other(anyhow::anyhow!("create schema_migrations: {e}")))?;

    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, description) VALUES (0, 'baseline')",
        (),
    )
    .await
    .map_err(|e| Error::Other(anyhow::anyhow!("stamp baseline: {e}")))?;

    Ok(())
}

/// Execute each statement in `sql`. Uses a small state machine to split on
/// `;` correctly — the naive `str::split(';')` mis-handles `;` inside `--`
/// comments ("R2 object; no per-user rows here.") and `;` inside string
/// literals (`INSERT ... VALUES (9, 'a; b')`), both of which appear in our
/// migrations.
async fn run_sql_script(conn: &libsql::Connection, sql: &str, label: &str) -> Result<()> {
    for stmt in split_sql_statements(sql) {
        conn.execute(&stmt, ())
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!("{label}: {stmt}\n→ {e}")))?;
    }
    Ok(())
}

fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            // `--` line comment: skip to end of line.
            '-' if chars.peek() == Some(&'-') => {
                chars.next();
                while let Some(next) = chars.next() {
                    if next == '\n' {
                        current.push('\n');
                        break;
                    }
                }
            }
            // String literal: copy through the closing quote, honoring `''`
            // escapes so embedded `;` stays part of the statement.
            '\'' => {
                current.push(c);
                while let Some(inner) = chars.next() {
                    current.push(inner);
                    if inner == '\'' {
                        if chars.peek() == Some(&'\'') {
                            current.push(chars.next().unwrap());
                        } else {
                            break;
                        }
                    }
                }
            }
            ';' => {
                let stmt = current.trim().to_string();
                if !stmt.is_empty() {
                    statements.push(stmt);
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }

    let tail = current.trim();
    if !tail.is_empty() {
        statements.push(tail.to_string());
    }
    statements
}

/// Reset every row in the shared test-Turso database so the next test starts
/// from a blank slate. Preserves the `schema_migrations` table so the DB
/// doesn't look un-migrated after a wipe.
///
/// The order matters: child tables come before parent tables so FK cascades
/// don't fire redundantly. `libsql::Connection` doesn't expose transactions
/// for remote databases here, so we rely on CASCADE and delete in order.
pub async fn wipe_remote(remote: &crate::db::remote::RemoteDb) -> Result<()> {
    // A ~4-minute serialized test run leaves the shared libsql `Database`
    // handle idle between scenarios. Turso / intermediate hops occasionally
    // tear that TCP connection down, and the next operation surfaces as
    // "Connection reset by peer" or "stream not found". Force a fresh
    // handle at the start of every wipe so each scenario starts clean.
    remote.reconnect().await?;

    // Tables that reference others first, then roots. The list covers the
    // base schema + every table added by migrations 000001–000015.
    let tables = [
        "message_reaction",
        "group_invite",
        "group_join_request",
        "user_preferences",
        "user_block",
        "dm_channel_member",
        "dm_channel",
        "message_envelope",
        "conversation_watermark",
        "mls_commit_log",
        "mls_welcome",
        "mls_key_package",
        "mls_group_info",
        "device_enrollment_request",
        "security_event",
        "account_recovery",
        "user_device",
        "channels",
        "group_member",
        "groups",
        "attachment_object",
        "users",
    ];
    for t in tables {
        let mut attempts = 0;
        loop {
            let conn = remote.conn().await?;
            match conn.execute(&format!("DELETE FROM {t}"), ()).await {
                Ok(_) => break,
                Err(e) if attempts < 2 && crate::db::remote::is_transient_libsql_error(&e) => {
                    eprintln!("wipe {t}: transient libsql error, reconnecting and retrying: {e}");
                    remote.reconnect().await?;
                    attempts += 1;
                }
                Err(e) => return Err(Error::Other(anyhow::anyhow!("wipe {t}: {e}"))),
            }
        }
    }
    Ok(())
}
