//! Integration-test harness: N headless clients, each a real `tauri::App` on `MockRuntime`.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use tauri::test::{mock_builder, mock_context, noop_assets, MockRuntime, INVOKE_KEY};
use tauri::{App, WebviewWindow, WebviewWindowBuilder};

use crate::error::{Error, Result};
use crate::state::AppState;

/// Headless app with the full production command surface; not `run()`, tests call [`invoke`].
pub fn build_client_app(state: Arc<AppState>) -> Result<(App<MockRuntime>, WebviewWindow<MockRuntime>)> {
    let app = mock_builder()
        .invoke_handler(tauri::generate_handler![
            crate::commands::auth::initialize_identity,
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
            crate::commands::auth::is_current_device_revoked,
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
            crate::commands::device_link::create_device_link,
            crate::commands::device_link::poll_device_link,
            crate::commands::device_link::await_device_link,
            crate::commands::device_link::approve_device_link,
            crate::commands::device_link::cancel_device_link,
            crate::commands::device_link::claim_device_link,
            crate::commands::device_link::create_device_offer,
            crate::commands::device_link::await_device_offer,
            crate::commands::device_link::cancel_device_offer,
            crate::commands::device_link::accept_device_offer,
            crate::commands::device_link::finish_device_offer,
            crate::commands::device_enrollment::list_security_events,
            crate::commands::safety::get_safety_number,
            crate::commands::safety::set_contact_verified,
            crate::commands::safety::list_peer_verifications,
            crate::commands::safety::list_key_change_notices,
            crate::commands::user::get_user_profile,
            crate::commands::user::update_user_profile,
            crate::commands::user::search_user_by_username,
            crate::commands::user::get_preferences,
            crate::commands::user::save_preferences,
            crate::commands::groups::list_user_groups,
            crate::commands::groups::list_user_groups_with_channels,
            crate::commands::groups::list_group_channels,
            crate::commands::groups::list_muted_channels,
            crate::commands::groups::set_channel_muted,
            crate::commands::groups::create_group,
            crate::commands::groups::create_channel,
            crate::commands::groups::send_group_invite,
            crate::commands::groups::get_pending_invites,
            crate::commands::groups::accept_group_invite,
            crate::commands::groups::decline_group_invite,
            crate::commands::groups::list_group_invites,
            crate::commands::groups::revoke_group_invite,
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
            // #917: emoji LIST is members-only; RENDER deliberately is not (#848).
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
            crate::commands::blocks::report_user,
            crate::commands::blocks::list_blocked_users,
            crate::commands::bookmarks::save_message,
            crate::commands::bookmarks::unsave_message,
            crate::commands::bookmarks::toggle_saved_message,
            crate::commands::bookmarks::list_saved_messages,
            crate::commands::bookmarks::resolve_message_permalink,
            crate::commands::export::export_archive,
            crate::commands::export::fetch_export_attachments,
            // Path-taking shims; `tests/flows/security.rs` proves they refuse unchosen paths.
            crate::commands::r2::upload_media,
            crate::commands::emoji::upload_group_emoji,
            // For the vault soak suite (#1491).
            crate::commands::r2::get_media_url,
            crate::commands::r2::save_media_to_path,
            crate::commands::pinned_messages::pin_message,
            crate::commands::pinned_messages::unpin_message,
            crate::commands::pinned_messages::list_pinned_messages,
            crate::commands::file_keys::test_file_key_version,
            crate::commands::vault::get_vault_messages,
            crate::commands::vault::send_vault_message,
            crate::commands::vault::edit_vault_message,
            crate::commands::vault::set_vault_message_pinned,
            crate::commands::vault::delete_vault_message,
            crate::commands::vault::search_vault_messages,
            crate::commands::messages::list_messages,
            crate::commands::messages::send_message,
            crate::commands::messages::new_message_id,
            crate::commands::messages::retry_message,
            crate::commands::messages::discard_unsent_message,
            crate::commands::messages::flush_outbox,
            crate::commands::messages::get_channel_messages,
            crate::commands::messages::get_dm_messages,
            crate::commands::messages::ingest_channel_envelopes,
            crate::commands::messages::ingest_dm_envelopes,
            crate::commands::messages::read_channel_messages,
            crate::commands::messages::read_dm_messages,
            crate::commands::messages::read_last_messages,
            crate::commands::messages::read_thread_messages,
            crate::commands::messages::list_thread_summaries,
            crate::commands::messages::search_messages,
            crate::commands::messages::rebuild_search_index,
            crate::commands::messages::read_messages_around,
            crate::commands::messages::read_messages_after,
            crate::commands::messages::add_reaction,
            crate::commands::messages::remove_reaction,
            crate::commands::messages::get_reactions,
            crate::commands::messages::mark_messages_read,
            crate::commands::messages::get_unread_counts,
            crate::commands::messages::mark_conversation_read,
            crate::commands::messages::sync_read_cursors,
            crate::commands::messages::refresh_read_cursors,
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
        // Empty: the harness drives no picker, so path-taking shims must refuse every path.
        .manage(crate::pathscope::PathScope::default())
        .build(mock_context(noop_assets()))
        .map_err(|e| Error::Other(anyhow::anyhow!("build mock app: {e}")))?;

    let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .map_err(|e| Error::Other(anyhow::anyhow!("build webview: {e}")))?;

    Ok((app, webview))
}

/// Dispatch through the IPC pipeline on a blocking thread (`get_ipc_response` uses std mpsc).
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

pub async fn invoke_unit(
    webview: &WebviewWindow<MockRuntime>,
    cmd: &str,
    args: serde_json::Value,
) -> std::result::Result<(), String> {
    let _: serde_json::Value = invoke(webview, cmd, args).await?;
    Ok(())
}

use pollis_core::db::{
    BASELINE_SQL as BASELINE, LOG_DB_SCHEMA, POST_BASELINE_LOG_MIGRATIONS, POST_BASELINE_MIGRATIONS,
};

/// MLS control-plane tables that live ONLY on the commit-log DB, so a main-side query fails loudly (#420).
pub const LOG_TABLES: [&str; 3] = ["mls_commit_log", "mls_welcome", "mls_group_info"];

/// Apply the commit-log DB schema to a separate libsql file. Idempotent.
pub async fn bootstrap_log_schema(conn: &libsql::Connection) -> Result<()> {
    run_sql_script(conn, LOG_DB_SCHEMA, "log-db schema").await?;
    // Fresh DB and idempotent migrations, so apply unconditionally.
    for (_version, description, sql) in POST_BASELINE_LOG_MIGRATIONS {
        run_sql_script(conn, sql, &format!("log migration {description}")).await?;
    }
    Ok(())
}

/// Drop the log tables from the MAIN test DB so a misrouted query fails with "no such table".
pub async fn drop_log_tables_from_main(conn: &libsql::Connection) -> Result<()> {
    for t in LOG_TABLES {
        conn.execute(&format!("DROP TABLE IF EXISTS {t}"), ())
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!("drop {t} from main: {e}")))?;
    }
    Ok(())
}

pub async fn bootstrap_schema(conn: &libsql::Connection) -> Result<()> {
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

/// Split on `;` outside `--` comments and string literals (both appear in our migrations).
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
                // A `CREATE TRIGGER` body's inner `;` are not boundaries; it ends at `END;`.
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

/// Reset every row in both test DBs, preserving `schema_migrations`.
pub async fn wipe_remote(remote: &libsql::Connection, log: &libsql::Connection) -> Result<()> {
    // Children before parents; the MLS control-plane tables are wiped on `log` below.
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
        "pinned_message",
        "pin_keystate",
        "vault_attachment_ref",
        "vault_message",
        "channels",
        "group_member",
        "groups",
        "attachment_object",
        "users",
    ];
    wipe_tables(remote, &main_tables).await?;

    wipe_tables(log, &LOG_TABLES).await?;
    wipe_tables(log, &["mls_commit_since"]).await?;

    Ok(())
}

async fn wipe_tables(conn: &libsql::Connection, tables: &[&str]) -> Result<()> {
    for t in tables {
        conn.execute(&format!("DELETE FROM {t}"), ())
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!("wipe {t}: {e}")))?;
    }
    Ok(())
}
