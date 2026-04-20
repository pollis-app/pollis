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
// The test Turso instance starts empty. We need to apply the base schema +
// every numbered migration before any test can run. The production
// `pnpm db:push` / `pnpm db:migrate` binaries aren't in-tree right now, so
// the harness embeds the migrations itself with `include_str!` and tracks
// applied versions in `schema_migrations` the same way production does.

const BASE_SCHEMA: &str = include_str!("db/migrations/remote_schema.sql");

/// (version, sql) for every numbered migration. Ordering matters — run them
/// in ascending version order and skip any whose version already exists in
/// `schema_migrations`.
const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("db/migrations/000001_unique_username.sql")),
    (2, include_str!("db/migrations/000002_envelope_delivered_index.sql")),
    (3, include_str!("db/migrations/000003_mls.sql")),
    (4, include_str!("db/migrations/000004_drop_signal.sql")),
    (5, include_str!("db/migrations/000005_watermark.sql")),
    (6, include_str!("db/migrations/000006_voice_presence.sql")),
    (7, include_str!("db/migrations/000007_attachment_object.sql")),
    (8, include_str!("db/migrations/000008_admin_roles.sql")),
    (9, include_str!("db/migrations/000009_cleanup.sql")),
    (10, include_str!("db/migrations/000010_envelope_edit_type.sql")),
    (11, include_str!("db/migrations/000011_user_device.sql")),
    (12, include_str!("db/migrations/000012_voice_presence_unique.sql")),
    (
        13,
        include_str!("db/migrations/000013_account_identity_and_enrollment.sql"),
    ),
    (14, include_str!("db/migrations/000014_commit_metadata.sql")),
    (
        15,
        include_str!("db/migrations/000015_dm_requests_and_blocks.sql"),
    ),
    (
        16,
        include_str!("db/migrations/000016_watermark_device_scope.sql"),
    ),
    (
        18,
        include_str!("db/migrations/000018_drop_voice_presence.sql"),
    ),
];

/// Apply the base schema + any missing migrations to the shared test DB.
/// Idempotent: safe to call on every test run.
pub async fn bootstrap_schema(remote: &crate::db::remote::RemoteDb) -> Result<()> {
    let conn = remote.conn().await?;

    // Does the base schema exist? `users` is the oldest table and is never
    // dropped by a migration — use it as a marker.
    let has_base = conn
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

    if !has_base {
        run_sql_script(&conn, BASE_SCHEMA, "base schema").await?;
    }

    // `schema_migrations` is created by migration 1. Before applying it for
    // the first time there is nothing to probe — treat that as "nothing
    // applied".
    let has_migrations_table = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='schema_migrations'",
            (),
        )
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("probe schema_migrations: {e}")))?
        .next()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("probe schema_migrations row: {e}")))?
        .is_some();

    let mut applied: std::collections::HashSet<i64> = std::collections::HashSet::new();
    if has_migrations_table {
        let mut rows = conn
            .query("SELECT version FROM schema_migrations", ())
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!("select schema_migrations: {e}")))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!("schema_migrations row: {e}")))?
        {
            let v: i64 = row
                .get(0)
                .map_err(|e| Error::Other(anyhow::anyhow!("schema_migrations version: {e}")))?;
            applied.insert(v);
        }
    }

    for (version, sql) in MIGRATIONS {
        if applied.contains(version) {
            continue;
        }
        run_sql_script(&conn, sql, &format!("migration {version:04}")).await?;
    }

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
