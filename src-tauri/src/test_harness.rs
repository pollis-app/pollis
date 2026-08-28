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
/// (distinct keystore, distinct `device_id`, distinct local DB handle). Since
/// #987 they share no database handle at all — there is none to share: each
/// reaches the same in-process Delivery Service over HTTP, exactly as a shipped
/// client reaches the real one.
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
            crate::commands::auth::request_email_change_otp,
            crate::commands::auth::verify_email_change,
            crate::commands::auth::dev_login,
            crate::commands::auth::get_session,
            crate::commands::auth::logout,
            crate::commands::auth::delete_account,
            crate::commands::auth::list_known_accounts,
            crate::commands::auth::wipe_local_data,
            crate::commands::auth::list_user_devices,
            crate::commands::auth::revoke_device,
            crate::commands::auth::is_current_device_registered,
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
            crate::commands::device_enrollment::finalize_device_enrollment,
            crate::commands::device_enrollment::list_security_events,
            crate::commands::safety::get_safety_number,
            crate::commands::safety::set_contact_verified,
            crate::commands::safety::list_peer_verifications,
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
            crate::commands::groups::create_group_invite_link,
            crate::commands::groups::list_group_invite_links,
            crate::commands::groups::revoke_group_invite_link,
            crate::commands::groups::redeem_group_invite_link,
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
            // #917: the emoji LIST is members-only while the emoji RENDER path
            // deliberately is not (#848). Both are registered here so the
            // integration suite can hold that pair apart — the guard and the
            // thing the guard must not break are only meaningful together.
            crate::commands::emoji::list_group_emoji,
            crate::commands::emoji::list_usable_emoji,
            crate::commands::emoji::get_emoji_url,
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
            crate::commands::bookmarks::save_message,
            crate::commands::bookmarks::unsave_message,
            crate::commands::bookmarks::toggle_saved_message,
            crate::commands::bookmarks::list_saved_messages,
            crate::commands::bookmarks::resolve_message_permalink,
            crate::commands::messages::list_messages,
            crate::commands::messages::send_message,
            crate::commands::messages::get_channel_messages,
            crate::commands::messages::get_dm_messages,
            crate::commands::messages::read_channel_messages,
            crate::commands::messages::read_dm_messages,
            crate::commands::messages::read_last_messages,
            crate::commands::messages::read_thread_messages,
            crate::commands::messages::list_thread_summaries,
            crate::commands::messages::search_messages,
            crate::commands::messages::rebuild_search_index,
            crate::commands::messages::read_messages_around,
            crate::commands::messages::add_reaction,
            crate::commands::messages::remove_reaction,
            crate::commands::messages::get_reactions,
            crate::commands::messages::mark_messages_read,
            crate::commands::messages::get_unread_counts,
            crate::commands::messages::mark_conversation_read,
            crate::commands::messages::sync_read_cursors,
            crate::commands::messages::get_conversation_receipts,
            crate::commands::messages::delete_message,
            crate::commands::messages::edit_message,
            crate::commands::mls::poll_mls_welcomes,
            crate::commands::mls::process_pending_commits,
            crate::commands::mls::catch_up_all_mls_groups,
            crate::commands::overlay::get_overlay_mode,
            crate::commands::overlay::set_overlay_mode,
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
            // Must match the platform webview origin tauri 2.11's ACL
            // resolves capabilities against (see tauri::test docs).
            url: if cfg!(any(windows, target_os = "android")) {
                "http://tauri.localhost"
            } else {
                "tauri://localhost"
            }
            .parse()
            .unwrap(),
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

use pollis_core::db::{
    BASELINE_SQL as BASELINE, LOG_DB_SCHEMA, POST_BASELINE_LOG_MIGRATIONS, POST_BASELINE_MIGRATIONS,
};

/// The three MLS control-plane tables that live ONLY on the commit-log DB.
/// They are created by the main-DB baseline (for old shipped clients), then
/// dropped from the main test DB by [`drop_log_tables_from_main`] so that — as
/// in #420 production — a stray main-side read/write of them fails loudly
/// instead of silently succeeding on a shared file.
pub const LOG_TABLES: [&str; 3] = ["mls_commit_log", "mls_welcome", "mls_group_info"];

/// Apply the commit-log DB schema (the three MLS control-plane tables) to a
/// genuinely separate libsql file. Idempotent (`CREATE TABLE IF NOT EXISTS`).
pub async fn bootstrap_log_schema(conn: &libsql::Connection) -> Result<()> {
    run_sql_script(conn, LOG_DB_SCHEMA, "log-db schema").await?;
    // Post-baseline log-DB migrations (mirrors db-apply.sh's second apply). The
    // log DB is fresh per test run and each migration is idempotent
    // (CREATE ... IF NOT EXISTS + a dedupe DELETE that's a no-op on empty data),
    // so applying them unconditionally matches the prod schema without gating.
    for (_version, description, sql) in POST_BASELINE_LOG_MIGRATIONS {
        run_sql_script(conn, sql, &format!("log migration {description}")).await?;
    }
    Ok(())
}

/// Drop the three MLS control-plane tables from the MAIN test DB. The baseline
/// creates them (they still ship in the main schema for old clients), but in
/// the split-DB harness they must live ONLY on the log DB so a misrouted query
/// — a `log_db` table read on the main connection — fails with "no such table".
pub async fn drop_log_tables_from_main(conn: &libsql::Connection) -> Result<()> {
    for t in LOG_TABLES {
        conn.execute(&format!("DROP TABLE IF EXISTS {t}"), ())
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!("drop {t} from main: {e}")))?;
    }
    Ok(())
}

/// Apply the baseline schema to the shared test DB if it hasn't been applied
/// yet. Idempotent: safe to call on every test run.
pub async fn bootstrap_schema(conn: &libsql::Connection) -> Result<()> {
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
        run_sql_script(conn, BASELINE, "baseline").await?;
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

    // Apply post-baseline migrations the harness knows about. Each migration
    // is gated on schema_migrations so reruns are no-ops.
    for (version, description, sql) in POST_BASELINE_MIGRATIONS {
        let already = conn
            .query(
                "SELECT 1 FROM schema_migrations WHERE version = ?1",
                [libsql::Value::Integer(i64::from(*version))],
            )
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!("probe schema_migrations v{version}: {e}")))?
            .next()
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!("probe schema_migrations row v{version}: {e}")))?
            .is_some();
        if already {
            continue;
        }
        run_sql_script(conn, sql, &format!("migration {version}_{description}")).await?;
        conn.execute(
            "INSERT INTO schema_migrations (version, description) VALUES (?1, ?2)",
            (
                libsql::Value::Integer(i64::from(*version)),
                libsql::Value::Text(description.to_string()),
            ),
        )
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("stamp v{version}: {e}")))?;
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
                // `by_ref()` so the outer loop resumes from where this stops —
                // the same semantics as the `while let` this replaces, which
                // clippy rejects (`while_let_on_iterator`) under `-D warnings`
                // whenever the crate is built with `test-harness`.
                for next in chars.by_ref() {
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
                // A `CREATE TRIGGER` body carries its own statement-terminating
                // `;` (`BEGIN <stmt>; END`). Those inner `;` are NOT statement
                // boundaries — the trigger ends only at the `;` after its `END`.
                // Keep accumulating until the trimmed text ends with `END`.
                let upper = current.trim().to_ascii_uppercase();
                if upper.starts_with("CREATE TRIGGER") && !upper.ends_with("END") {
                    current.push(c);
                } else {
                    let stmt = current.trim().to_string();
                    if !stmt.is_empty() {
                        statements.push(stmt);
                    }
                    current.clear();
                }
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
/// don't fire redundantly, and we rely on CASCADE rather than a transaction.
///
/// Takes bare connections since #987: the harness's "remote" is a local libsql
/// file owned by the in-process DS, so the reconnect-on-transient-error dance
/// this used to do — a defence against Turso tearing an idle TCP connection
/// down mid-run — has nothing left to defend against.
pub async fn wipe_remote(remote: &libsql::Connection, log: &libsql::Connection) -> Result<()> {
    // MAIN DB: tables that reference others first, then roots. The list covers
    // the base schema + every table added by migrations 000001–000015. The
    // three MLS control-plane tables (`mls_commit_log`, `mls_welcome`,
    // `mls_group_info`) are deliberately ABSENT — they live only on the log DB
    // now (dropped from main by `drop_log_tables_from_main`), so a DELETE here
    // would itself be a "no such table" failure. They're wiped on `log` below.
    let main_tables = [
        "message_reaction",
        "group_invite",
        "group_join_request",
        "user_preferences",
        "user_block",
        "dm_channel_member",
        "dm_channel",
        "message_envelope",
        "conversation_watermark",
        "mls_key_package",
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
    wipe_tables(remote, &main_tables).await?;

    // LOG DB: the three MLS control-plane tables live here and ONLY here, plus
    // the per-device retention high-water (`mls_commit_since`, #539).
    wipe_tables(log, &LOG_TABLES).await?;
    wipe_tables(log, &["mls_commit_since"]).await?;

    Ok(())
}

/// DELETE every row from `tables` on `conn`. Shared by the main- and log-DB
/// wipes.
async fn wipe_tables(conn: &libsql::Connection, tables: &[&str]) -> Result<()> {
    for t in tables {
        conn.execute(&format!("DELETE FROM {t}"), ())
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!("wipe {t}: {e}")))?;
    }
    Ok(())
}
