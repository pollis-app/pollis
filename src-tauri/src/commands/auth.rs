use serde::{Deserialize, Serialize};
use tauri::State;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Result;
use crate::keystore;
use crate::state::AppState;
use ulid::Ulid;

const SESSION_KEY: &str = "session";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserProfile {
    pub id: String,
    pub email: String,
    pub username: String,
}

#[derive(Debug, Serialize)]
pub struct IdentityInfo {
    pub user_id: String,
    pub public_key: String,
    pub is_new: bool,
}

/// Ensure this user has MLS credentials set up and a KeyPackage published.
/// Called after login to make the user invitable to MLS groups/DMs.
#[tauri::command]
pub async fn initialize_identity(
    state: State<'_, Arc<AppState>>,
    user_id: String,
) -> Result<IdentityInfo> {
    match crate::commands::mls::ensure_mls_key_package(&state, &user_id).await {
        Ok(()) => eprintln!("[identity] MLS key package ensured for {user_id}"),
        Err(e) => eprintln!("[identity] MLS key package error (non-fatal): {e}"),
    }

    Ok(IdentityInfo { user_id, public_key: String::new(), is_new: false })
}

/// Check whether an MLS identity exists locally.
#[tauri::command]
pub async fn get_identity() -> Result<Option<IdentityInfo>> {
    Ok(None)
}

/// Request an OTP to be sent to the given email address.
#[tauri::command]
pub async fn request_otp(
    state: State<'_, Arc<AppState>>,
    email: String,
) -> Result<()> {
    use rand::Rng;
    use sha2::{Sha256, Digest};

    // Generate OTP in a scoped block so ThreadRng (non-Send) is dropped
    // before the first await point.
    let otp = {
        let mut rng = rand::thread_rng();
        format!("{:06}", rng.gen_range(0..1_000_000u32))
    };

    let mut hasher = Sha256::new();
    hasher.update(otp.as_bytes());
    let hash = format!("{:x}", hasher.finalize());

    let expires_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() + 600;

    {
        let mut store = state.otp_store.lock().await;
        store.insert(email.clone(), crate::state::OtpEntry { hash, expires_at });
    }

    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "from": "Pollis <noreply@mail.pollis.com>",
        "to": [email],
        "subject": "Your Pollis sign-in code",
        "text": format!("Your verification code is: {}\n\nThis code expires in 10 minutes.", otp),
    });

    let resp = client
        .post("https://api.resend.com/emails")
        .header("Authorization", format!("Bearer {}", state.config.resend_api_key))
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let err_text = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("Failed to send email: {}", err_text).into());
    }

    Ok(())
}

/// Verify an OTP code, create or load the user in Turso, persist session to keystore.
#[tauri::command]
pub async fn verify_otp(
    state: State<'_, Arc<AppState>>,
    email: String,
    code: String,
) -> Result<UserProfile> {
    use sha2::{Sha256, Digest};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let stored = {
        let store = state.otp_store.lock().await;
        store.get(&email).cloned()
    };

    let entry = stored.ok_or_else(|| {
        anyhow::anyhow!("No sign-in request found for this email. Please request a new code.")
    })?;

    if now > entry.expires_at {
        return Err(anyhow::anyhow!("This code has expired. Please request a new one.").into());
    }

    let mut hasher = Sha256::new();
    hasher.update(code.trim().as_bytes());
    let provided_hash = format!("{:x}", hasher.finalize());

    if provided_hash != entry.hash {
        return Err(anyhow::anyhow!("Invalid code. Please check and try again.").into());
    }

    {
        let mut store = state.otp_store.lock().await;
        store.remove(&email);
    }

    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT id, username FROM users WHERE email = ?1",
        libsql::params![email.clone()],
    ).await?;

    let user_row = rows.next().await?;

    let (user_id, username) = if let Some(row) = user_row {
        let id: String = row.get(0)?;
        let uname: String = row.get(1).unwrap_or_else(|_| {
            email.split('@').next().unwrap_or("user").to_string()
        });
        (id, uname)
    } else {
        let user_id = Ulid::new().to_string();
        // Append the last 4 chars of the ULID so the default username is unique
        // even when multiple users share the same email prefix (e.g. john@foo.com, john@bar.com).
        let suffix = &user_id[user_id.len().saturating_sub(4)..];
        let email_prefix = email.split('@').next().unwrap_or("user");
        let default_username = format!("{}_{}", email_prefix, suffix);
        conn.execute(
            "INSERT INTO users (id, email, username) VALUES (?1, ?2, ?3)",
            libsql::params![user_id.clone(), email.clone(), default_username.clone()],
        ).await?;
        (user_id, default_username)
    };

    let profile = UserProfile { id: user_id, email, username };

    // Persist session to OS keystore so it survives app restarts
    let session_bytes = serde_json::to_vec(&profile)
        .map_err(|e| anyhow::anyhow!("Failed to serialize session: {e}"))?;
    keystore::store_for_user(SESSION_KEY, &profile.id, &session_bytes).await?;
    state.load_user_db(&profile.id).await?;
    crate::accounts::upsert_account(&profile.id, &profile.username, None)?;

    Ok(profile)
}

/// Load the persisted session from the OS keystore.
/// Verifies the user still exists in Turso — if not, clears the stale session and returns None.
/// Returns None if the user has never signed in or has logged out.
#[tauri::command]
pub async fn get_session(state: State<'_, Arc<AppState>>) -> Result<Option<UserProfile>> {
    // Identify the last active user from the local accounts index.
    let index = crate::accounts::read_accounts_index();
    let user_id = match index.last_active_user {
        Some(uid) => uid,
        None => return Ok(None),
    };

    let bytes = match keystore::load_for_user(SESSION_KEY, &user_id).await? {
        Some(b) => b,
        None => return Ok(None),
    };

    let profile: UserProfile = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("Failed to deserialize session: {e}"))?;

    // Verify the user still exists in Turso. After a DB wipe or account deletion
    // the keystore still has the old profile, which would cause FK errors everywhere.
    // Only clear the session if Turso definitively confirms the user is gone.
    // Network errors are treated as "assume valid" so a flaky connection at startup
    // doesn't force the user to re-authenticate every time.
    match state.remote_db.conn().await {
        Ok(conn) => {
            match conn.query(
                "SELECT id FROM users WHERE id = ?1",
                libsql::params![profile.id.clone()],
            ).await {
                Ok(mut rows) => {
                    match rows.next().await {
                        Ok(None) => {
                            // Turso confirmed the user doesn't exist — stale session
                            let _ = keystore::delete_for_user(SESSION_KEY, &user_id).await;
                            let _ = crate::accounts::clear_last_active_user();
                            return Ok(None);
                        }
                        Ok(Some(_)) => {
                            // User confirmed to exist — session is valid
                        }
                        Err(e) => {
                            eprintln!("[session] failed to read Turso row ({e}); using cached session");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[session] Turso query failed ({e}); using cached session");
                }
            }
        }
        Err(e) => {
            eprintln!("[session] Turso connection failed ({e}); using cached session");
        }
    }

    // Open the per-user local database.
    state.load_user_db(&profile.id).await?;

    Ok(Some(profile))
}

/// Clear the persisted session (logout). Optionally wipe the per-user DB and identity keys.
#[tauri::command]
pub async fn logout(state: State<'_, Arc<AppState>>, delete_data: bool) -> Result<()> {
    let index = crate::accounts::read_accounts_index();
    let user_id = index.last_active_user;

    if let Some(ref uid) = user_id {
        let _ = keystore::delete_for_user(SESSION_KEY, uid).await;
    }

    state.unload_user_db().await;

    if delete_data {
        if let Some(ref uid) = user_id {
            let _ = keystore::delete("identity_key_private").await;
            let _ = keystore::delete("identity_key_public").await;
            let _ = keystore::delete_for_user("db_key", uid).await;
            let data_dir = crate::db::local::dirs_path();
            let db_path = data_dir.join(format!("pollis_{uid}.db"));
            if db_path.exists() {
                let _ = std::fs::remove_file(&db_path);
            }
            let _ = std::fs::remove_file(data_dir.join(format!("pollis_{uid}.db-wal")));
            let _ = std::fs::remove_file(data_dir.join(format!("pollis_{uid}.db-shm")));
            let _ = crate::accounts::remove_account(uid);
        }
    } else {
        let _ = crate::accounts::clear_last_active_user();
    }

    Ok(())
}

/// Permanently delete the account: wipe all remote data, clear keystore, delete local DB.
#[tauri::command]
pub async fn delete_account(
    state: State<'_, Arc<AppState>>,
    user_id: String,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Remove encrypted message envelopes sent by this user
    let _ = conn.execute(
        "DELETE FROM message_envelope WHERE sender_id = ?1",
        libsql::params![user_id.clone()],
    ).await;

    // Remove MLS key packages
    let _ = conn.execute(
        "DELETE FROM mls_key_package WHERE user_id = ?1",
        libsql::params![user_id.clone()],
    ).await;

    // Remove group memberships
    let _ = conn.execute(
        "DELETE FROM group_member WHERE user_id = ?1",
        libsql::params![user_id.clone()],
    ).await;

    // Remove the user row itself
    conn.execute(
        "DELETE FROM users WHERE id = ?1",
        libsql::params![user_id.clone()],
    ).await?;

    // Close and delete the per-user local database
    state.unload_user_db().await;
    {
        let data_dir = crate::db::local::dirs_path();
        let db_path = data_dir.join(format!("pollis_{user_id}.db"));
        if db_path.exists() {
            let _ = std::fs::remove_file(&db_path);
        }
        let _ = std::fs::remove_file(data_dir.join(format!("pollis_{user_id}.db-wal")));
        let _ = std::fs::remove_file(data_dir.join(format!("pollis_{user_id}.db-shm")));
    }

    // Clear all keystore entries
    let _ = keystore::delete_for_user(SESSION_KEY, &user_id).await;
    let _ = keystore::delete_for_user("db_key", &user_id).await;

    // Remove from local accounts index
    let _ = crate::accounts::remove_account(&user_id);

    eprintln!("[account] deleted account for user {user_id}");
    Ok(())
}

/// Return the list of accounts that have previously signed in on this device.
/// Used by the login screen to show a "continue as" picker.
#[tauri::command]
pub fn list_known_accounts() -> crate::accounts::AccountsIndex {
    crate::accounts::read_accounts_index()
}

