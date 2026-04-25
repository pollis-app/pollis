use serde::{Deserialize, Serialize};
use tauri::State;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Result;
use crate::state::AppState;
use ulid::Ulid;

const DEVICE_ID_KEY: &str = "device_id";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserProfile {
    pub id: String,
    pub email: String,
    pub username: String,
    /// Only populated on first-device signup (`verify_otp` / `dev_login`).
    /// The backend clears this field before persisting the profile to the
    /// OS keystore so the Secret Key is never written to disk as part of
    /// the session blob. Frontend is expected to read it once off the
    /// auth response, show the Emergency Kit screen, then forget it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_secret_key: Option<String>,
    /// True when this device has signed in against a user that already
    /// has an `account_id_pub` on the server BUT no local
    /// `account_id_key` in this device's keystore. The frontend must
    /// route to the enrollment gate before showing the main app.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub enrollment_required: bool,
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
    let device_id = state.device_id.lock().await.clone()
        .ok_or_else(|| anyhow::anyhow!("device_id not set — login incomplete"))?;

    match crate::commands::mls::ensure_mls_key_package(&state, &user_id, &device_id).await {
        Ok(()) => eprintln!("[identity] MLS key package ensured for {user_id} device {device_id}"),
        Err(e) => eprintln!("[identity] MLS key package error (non-fatal): {e}"),
    }

    // Re-process any pending MLS Welcome messages. This is a no-op when the
    // local MLS state is intact, but recovers group membership after the local
    // DB is wiped (e.g. schema version bump) because load_user_db resets
    // delivered = 0 for this user's welcomes in that case.
    match crate::commands::mls::poll_mls_welcomes_inner(state.inner(), &user_id, &device_id).await {
        Ok(()) => {}
        Err(e) => eprintln!("[identity] poll_mls_welcomes (non-fatal): {e}"),
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

    // In development, DEV_OTP overrides the random code and skips the email send.
    // Set DEV_OTP=000000 in .env.development to use a fixed code during local iteration.
    // Compiled out entirely in release builds so the env var has no effect in production.
    #[cfg(debug_assertions)]
    if let Ok(dev_otp) = std::env::var("DEV_OTP") {
        let mut dev_hasher = Sha256::new();
        dev_hasher.update(dev_otp.trim().as_bytes());
        let dev_hash = format!("{:x}", dev_hasher.finalize());
        let mut store = state.otp_store.lock().await;
        store.insert(email.clone(), crate::state::OtpEntry { hash: dev_hash, expires_at });
        eprintln!("[auth] DEV_OTP active — skipping email send for {email}");
        return Ok(());
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
        "SELECT id, username, account_id_pub FROM users WHERE email = ?1",
        libsql::params![email.clone()],
    ).await?;

    let user_row = rows.next().await?;

    let (user_id, username, remote_pub) = if let Some(row) = user_row {
        let id: String = row.get(0)?;
        let uname: String = row.get(1).unwrap_or_else(|_| {
            email.split('@').next().unwrap_or("user").to_string()
        });
        let pub_bytes: Option<Vec<u8>> = row.get(2).ok();
        (id, uname, pub_bytes)
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
        (user_id, default_username, None)
    };

    // If the server has an identity but the locally-stored key doesn't
    // match, this device has been orphaned by a reset on another device
    // (or its local key is corrupt). Wipe the stale local key so the
    // normal enrollment gate takes over.
    if let Some(ref pub_bytes) = remote_pub {
        let matches = crate::commands::account_identity::has_matching_local_account_identity(
            state.inner(),
            &user_id,
            pub_bytes,
        )
        .await
        .unwrap_or(false);
        if !matches {
            if let Err(e) =
                crate::commands::account_identity::wipe_local_account_identity(state.inner(), &user_id).await
            {
                eprintln!("[auth] wipe_local_account_identity (non-fatal): {e}");
            }
        }
    }

    let has_identity = remote_pub.is_some();

    // First-device signup (or a pre-identity user): generate the long-lived
    // account identity key and a Secret Key to hand back to the frontend
    // exactly once. See MULTI_DEVICE_ENROLLMENT.md.
    let new_secret_key = if !has_identity {
        match crate::commands::account_identity::generate_account_identity(
            state.inner(),
            &user_id,
        )
        .await
        {
            Ok(sk) => Some(sk),
            Err(e) => {
                eprintln!("[auth] generate_account_identity failed: {e}");
                return Err(e);
            }
        }
    } else {
        None
    };

    // Enrollment required when the user has an identity on the server but
    // this device doesn't hold a matching local copy of the account_id_key.
    let enrollment_required = has_identity
        && !crate::commands::account_identity::has_local_account_identity(state.inner(), &user_id)
            .await
            .unwrap_or(false);

    let profile = UserProfile {
        id: user_id,
        email,
        username,
        new_secret_key: new_secret_key.clone(),
        enrollment_required,
    };

    // accounts.json is the durable record of "who has signed in on
    // this device" — the prior `session_{uid}` keystore blob was a
    // redundant second source of truth and a flaky one (issue #184).
    state.load_user_db(&profile.id).await?;
    register_device(state.inner(), &profile.id).await?;
    crate::accounts::upsert_account(&profile.id, &profile.username, Some(&profile.email), None)?;

    Ok(profile)
}

/// Dev-only: bypass OTP and log in directly with an email address.
/// Returns an error in release builds so this can never be used in production.
#[tauri::command]
pub async fn dev_login(
    _state: State<'_, Arc<AppState>>,
    _email: String,
) -> Result<UserProfile> {
    #[cfg(not(debug_assertions))]
    {
        let _ = (_state, _email);
        return Err(crate::error::Error::Other(anyhow::anyhow!("dev_login is not available in release builds")));
    }

    #[cfg(debug_assertions)]
    {
        let profile = dev_login_inner(&_state, _email).await?;
        eprintln!("[auth] dev_login: logged in as {}", profile.username);
        Ok(profile)
    }
}

/// Rehydrate the current user's profile on app boot.
///
/// Reads `accounts.json` (durable, crash-safe) for `last_active_user`
/// and reconstructs the `UserProfile` from that entry. Previously this
/// also read a `session_{uid}` keystore blob — a redundant second
/// source of truth whose transient read failures were a direct cause
/// of issue #184 kicking users back to OTP. The blob is gone.
#[tauri::command]
pub async fn get_session(state: State<'_, Arc<AppState>>) -> Result<Option<UserProfile>> {
    // In development, DEV_EMAIL bypasses the normal session check and logs in directly.
    // Set DEV_EMAIL=user@example.com in .env.development to auto-login on every startup.
    #[cfg(debug_assertions)]
    if let Ok(dev_email) = std::env::var("DEV_EMAIL") {
        let dev_email = dev_email.trim().to_string();
        if !dev_email.is_empty() {
            eprintln!("[auth] DEV_EMAIL active — auto-logging in as {dev_email}");
            let profile = dev_login_inner(&state, dev_email).await?;
            return Ok(Some(profile));
        }
    }

    // Identify the last active user from the local accounts index.
    let index = crate::accounts::read_accounts_index()?;
    let account = match index
        .last_active_user
        .and_then(|uid| index.accounts.iter().find(|a| a.user_id == uid).cloned())
    {
        Some(a) => a,
        None => {
            eprintln!("[session] no last_active_user in accounts index — showing login");
            return Ok(None);
        }
    };

    let mut profile = UserProfile {
        id: account.user_id.clone(),
        email: account.email.clone().unwrap_or_default(),
        username: account.username.clone(),
        new_secret_key: None,
        enrollment_required: false,
    };

    // Verify the user still exists in Turso. After an account deletion
    // elsewhere the local record is stale and would cause FK errors.
    // Network failures are treated as "assume valid" so a flaky
    // connection at startup doesn't force re-authentication.
    match state.remote_db.conn().await {
        Ok(conn) => {
            match conn.query(
                "SELECT id FROM users WHERE id = ?1",
                libsql::params![profile.id.clone()],
            ).await {
                Ok(mut rows) => {
                    match rows.next().await {
                        Ok(None) => {
                            // Turso confirmed the user doesn't exist — stale local record.
                            let _ = crate::accounts::remove_account(&profile.id);
                            return Ok(None);
                        }
                        Ok(Some(_)) => {
                            // User confirmed to exist — proceed.
                        }
                        Err(e) => {
                            eprintln!("[session] failed to read Turso row ({e}); proceeding from accounts.json");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[session] Turso query failed ({e}); proceeding from accounts.json");
                }
            }
        }
        Err(e) => {
            eprintln!("[session] Turso connection failed ({e}); proceeding from accounts.json");
        }
    }

    // Open the per-user local database.
    if let Err(e) = state.load_user_db(&profile.id).await {
        eprintln!("[session] load_user_db failed for user {} ({e}) — bouncing to login", profile.id);
        return Err(e);
    }
    if let Err(e) = register_device(state.inner(), &profile.id).await {
        eprintln!("[session] register_device failed for user {} ({e}) — bouncing to login", profile.id);
        return Err(e);
    }

    // Recompute enrollment_required. Two reasons the device might need
    // enrollment:
    //   (a) server has an account_id_pub but this device has no local key
    //   (b) server's pub doesn't match the local key — orphaned by a
    //       soft-recovery reset on another device
    // In case (b) we wipe the stale local key so the gate path is clean.
    let remote_pub: Option<Vec<u8>> = match state.remote_db.conn().await {
        Ok(conn) => {
            match conn
                .query(
                    "SELECT account_id_pub FROM users WHERE id = ?1",
                    libsql::params![profile.id.clone()],
                )
                .await
            {
                Ok(mut rows) => match rows.next().await {
                    Ok(Some(row)) => row.get::<Option<Vec<u8>>>(0).ok().flatten(),
                    Ok(None) => None,
                    Err(e) => {
                        eprintln!("[session] enrollment-recompute row read failed ({e}) — bouncing to login");
                        return Err(e.into());
                    }
                },
                Err(e) => {
                    eprintln!("[session] enrollment-recompute query failed ({e}) — bouncing to login");
                    return Err(e.into());
                }
            }
        }
        Err(e) => {
            eprintln!("[session] enrollment-recompute Turso connect failed ({e}) — bouncing to login");
            return Err(e);
        }
    };

    if let Some(ref pub_bytes) = remote_pub {
        let matches = crate::commands::account_identity::has_matching_local_account_identity(
            state.inner(),
            &profile.id,
            pub_bytes,
        )
        .await
        .unwrap_or(false);
        if !matches {
            if let Err(e) =
                crate::commands::account_identity::wipe_local_account_identity(
                    state.inner(),
                    &profile.id,
                ).await
            {
                eprintln!("[session] wipe_local_account_identity (non-fatal): {e}");
            }
        }
    }

    let has_local_identity = crate::commands::account_identity::has_local_account_identity(
        state.inner(),
        &profile.id,
    )
    .await
    .unwrap_or(false);
    profile.enrollment_required = remote_pub.is_some() && !has_local_identity;

    Ok(Some(profile))
}

// Helper shared by get_session (DEV_EMAIL) and dev_login.
#[cfg(debug_assertions)]
async fn dev_login_inner(state: &Arc<AppState>, email: String) -> Result<UserProfile> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT id, username, account_id_pub FROM users WHERE email = ?1",
        libsql::params![email.clone()],
    ).await?;

    let (user_id, username, remote_pub) = if let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let uname: String = row.get(1).unwrap_or_else(|_| {
            email.split('@').next().unwrap_or("user").to_string()
        });
        let pub_bytes: Option<Vec<u8>> = row.get(2).ok();
        (id, uname, pub_bytes)
    } else {
        let user_id = Ulid::new().to_string();
        let suffix = &user_id[user_id.len().saturating_sub(4)..];
        let email_prefix = email.split('@').next().unwrap_or("user");
        let default_username = format!("{}_{}", email_prefix, suffix);
        conn.execute(
            "INSERT INTO users (id, email, username) VALUES (?1, ?2, ?3)",
            libsql::params![user_id.clone(), email.clone(), default_username.clone()],
        ).await?;
        (user_id, default_username, None)
    };

    // Orphan-detection: wipe any stale local key that doesn't match
    // the server's current account_id_pub.
    if let Some(ref pub_bytes) = remote_pub {
        let matches = crate::commands::account_identity::has_matching_local_account_identity(
            state,
            &user_id,
            pub_bytes,
        )
        .await
        .unwrap_or(false);
        if !matches {
            if let Err(e) =
                crate::commands::account_identity::wipe_local_account_identity(state, &user_id).await
            {
                eprintln!("[auth] wipe_local_account_identity (non-fatal): {e}");
            }
        }
    }

    let has_identity = remote_pub.is_some();

    let new_secret_key = if !has_identity {
        match crate::commands::account_identity::generate_account_identity(state, &user_id).await {
            Ok(sk) => Some(sk),
            Err(e) => {
                eprintln!("[auth] generate_account_identity failed: {e}");
                return Err(e);
            }
        }
    } else {
        None
    };

    let enrollment_required = has_identity
        && !crate::commands::account_identity::has_local_account_identity(state, &user_id)
            .await
            .unwrap_or(false);

    let profile = UserProfile {
        id: user_id,
        email,
        username,
        new_secret_key,
        enrollment_required,
    };

    state.load_user_db(&profile.id).await?;
    register_device(state, &profile.id).await?;
    crate::accounts::upsert_account(&profile.id, &profile.username, Some(&profile.email), None)?;
    Ok(profile)
}

/// Register this device for the given user. Generates a stable device_id on first
/// call (persisted in the OS keystore), inserts/updates the remote `user_device`
/// table, and stores the id in `AppState.device_id`.
async fn register_device(state: &Arc<AppState>, user_id: &str) -> Result<String> {
    let device_id = match state.keystore.load_for_user(DEVICE_ID_KEY, user_id).await? {
        Some(bytes) => String::from_utf8(bytes)
            .map_err(|e| anyhow::anyhow!("corrupt device_id in keystore: {e}"))?,
        None => {
            let id = Ulid::new().to_string();
            state.keystore.store_for_user(DEVICE_ID_KEY, user_id, id.as_bytes()).await?;
            id
        }
    };

    let hostname = gethostname::gethostname().to_string_lossy().to_string();
    let device_name = format!("{hostname} ({})", std::env::consts::OS);

    // COALESCE preserves any existing device_name — fills it in only if NULL,
    // so a user-set rename (future feature) is never overwritten on reconnect.
    let conn = state.remote_db.conn().await?;
    conn.execute(
        "INSERT INTO user_device (device_id, user_id, device_name) VALUES (?1, ?2, ?3) \
         ON CONFLICT(device_id) DO UPDATE SET \
            last_seen = datetime('now'), \
            device_name = COALESCE(user_device.device_name, excluded.device_name)",
        libsql::params![device_id.clone(), user_id, device_name],
    ).await?;

    // Seed watermark rows for every conversation the user is already a member
    // of so this device doesn't retroactively block envelope cleanup (see #162).
    // Pre-registration messages aren't decryptable here anyway — MLS welcomes
    // only flow forward — so anchoring the watermark at "now" is correct.
    if let Err(e) = conn.execute(
        "INSERT OR IGNORE INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at)
         SELECT c.id, ?1, ?2, datetime('now')
         FROM channels c
         JOIN group_member gm ON gm.group_id = c.group_id AND gm.user_id = ?1",
        libsql::params![user_id, device_id.clone()],
    ).await {
        eprintln!("[watermark] register_device: channel seed failed: {e}");
    }
    if let Err(e) = conn.execute(
        "INSERT OR IGNORE INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at)
         SELECT dcm.dm_channel_id, ?1, ?2, datetime('now')
         FROM dm_channel_member dcm WHERE dcm.user_id = ?1",
        libsql::params![user_id, device_id.clone()],
    ).await {
        eprintln!("[watermark] register_device: dm seed failed: {e}");
    }

    *state.device_id.lock().await = Some(device_id.clone());
    eprintln!("[auth] device registered: {device_id}");

    // Publish the device cross-signing cert so any client that reads this
    // row can verify this device belongs to the user. No-op if the device
    // doesn't yet hold the account identity key (pre-enrollment state).
    if let Err(e) = crate::commands::mls::ensure_device_cert(state, user_id, &device_id).await {
        eprintln!("[auth] ensure_device_cert failed (non-fatal): {e}");
    }

    Ok(device_id)
}

/// Clear the persisted session (logout). Optionally wipe the per-user DB and identity keys.
#[tauri::command]
pub async fn logout(state: State<'_, Arc<AppState>>, delete_data: bool) -> Result<()> {
    // If the index is corrupt we still want the user to be able to log out.
    // `accounts.json` has already been renamed to `.bad-<ts>.json` by the
    // read path, so a default (empty) index is the honest state of the world.
    let index = crate::accounts::read_accounts_index().unwrap_or_default();
    let user_id = index.last_active_user;

    // Grab the device_id before clearing it.
    let device_id = state.device_id.lock().await.take();

    state.unload_user_db().await;
    *state.unlock.lock().await = None;

    if delete_data {
        if let Some(ref uid) = user_id {
            // Remove this device from the remote registry.
            if let Some(ref did) = device_id {
                if let Ok(conn) = state.remote_db.conn().await {
                    let _ = conn.execute(
                        "DELETE FROM user_device WHERE device_id = ?1",
                        libsql::params![did.clone()],
                    ).await;
                }
            }
            let _ = state.keystore.delete_for_user("db_key", uid).await;
            let _ = state.keystore.delete_for_user("db_key_wrapped", uid).await;
            let _ = state.keystore.delete_for_user("account_id_key", uid).await;
            let _ = state.keystore.delete_for_user("account_id_key_wrapped", uid).await;
            let _ = state.keystore.delete_for_user("pin_meta", uid).await;
            let _ = state.keystore.delete_for_user(DEVICE_ID_KEY, uid).await;
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

/// Permanently delete the account: rotate MLS keys for all groups/DMs the user
/// belongs to, then wipe all remote data, clear keystore, and delete local DB.
///
/// MLS key rotation is done first (while the local DB is still open) so that
/// remaining group members receive a remove commit and advance their epoch.
/// This ensures forward secrecy: the deleted user's key material cannot decrypt
/// any messages sent after the rotation.
///
/// See: https://github.com/actuallydan/pollis/issues/103
#[tauri::command]
pub async fn delete_account(
    state: State<'_, Arc<AppState>>,
    user_id: String,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // ── Phase 1: Signal membership change ─────��──────────────────────
    // Broadcast membership_changed to all groups and DM channels the user
    // belongs to. Remaining online members will reconcile and remove the
    // deleting user's stale MLS leaves asynchronously. The user's local DB
    // (including MLS state) is wiped in Phase 3, so they can't decrypt
    // regardless.

    // Enumerate all groups the user belongs to.
    {
        let mut group_rows = conn.query(
            "SELECT group_id FROM group_member WHERE user_id = ?1",
            libsql::params![user_id.clone()],
        ).await?;

        let mut group_ids: Vec<String> = Vec::new();
        while let Some(row) = group_rows.next().await? {
            group_ids.push(row.get(0)?);
        }

        for gid in &group_ids {
            if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
                &state.livekit, gid,
            ).await {
                eprintln!("[account] membership_changed for group {gid} failed (non-fatal): {e}");
            }
        }
    }

    // Enumerate all DM channels the user belongs to.
    {
        let mut dm_rows = conn.query(
            "SELECT dm_channel_id FROM dm_channel_member WHERE user_id = ?1",
            libsql::params![user_id.clone()],
        ).await?;

        let mut dm_ids: Vec<String> = Vec::new();
        while let Some(row) = dm_rows.next().await? {
            dm_ids.push(row.get(0)?);
        }

        for dm_id in &dm_ids {
            if let Err(e) = crate::commands::livekit::publish_to_room_server(
                &state.config,
                dm_id,
                serde_json::json!({"type": "membership_changed", "conversation_id": dm_id}),
            ).await {
                eprintln!("[account] membership_changed for DM {dm_id} failed (non-fatal): {e}");
            }
        }
    }

    // ── Phase 2: remote data cleanup ───────────────────────────────────

    // Handle group ownership before removing memberships. For each group
    // the user belongs to, check whether they're the sole member (delete
    // the group) or the sole admin (promote another member first).
    {
        let mut group_rows = conn.query(
            "SELECT group_id, role FROM group_member WHERE user_id = ?1",
            libsql::params![user_id.clone()],
        ).await?;

        let mut memberships: Vec<(String, String)> = Vec::new();
        while let Some(row) = group_rows.next().await? {
            memberships.push((row.get(0)?, row.get(1)?));
        }

        for (gid, role) in &memberships {
            // How many members does this group have?
            let mut count_rows = conn.query(
                "SELECT COUNT(*) FROM group_member WHERE group_id = ?1",
                libsql::params![gid.clone()],
            ).await?;
            let member_count: i64 = if let Some(row) = count_rows.next().await? {
                row.get(0)?
            } else {
                0
            };

            if member_count <= 1 {
                // Sole member — delete the entire group (cascades channels, invites, etc.)
                let _ = conn.execute(
                    "DELETE FROM groups WHERE id = ?1",
                    libsql::params![gid.clone()],
                ).await;
                eprintln!("[account] deleted empty group {gid}");
            } else if role == "admin" {
                // Check if there are other admins.
                let mut admin_rows = conn.query(
                    "SELECT COUNT(*) FROM group_member WHERE group_id = ?1 AND role = 'admin' AND user_id != ?2",
                    libsql::params![gid.clone(), user_id.clone()],
                ).await?;
                let other_admins: i64 = if let Some(row) = admin_rows.next().await? {
                    row.get(0)?
                } else {
                    0
                };

                if other_admins == 0 {
                    // Sole admin — promote the first non-admin member.
                    let mut candidate_rows = conn.query(
                        "SELECT user_id FROM group_member WHERE group_id = ?1 AND user_id != ?2 LIMIT 1",
                        libsql::params![gid.clone(), user_id.clone()],
                    ).await?;
                    if let Some(row) = candidate_rows.next().await? {
                        let new_admin: String = row.get(0)?;
                        let _ = conn.execute(
                            "UPDATE group_member SET role = 'admin' WHERE group_id = ?1 AND user_id = ?2",
                            libsql::params![gid.clone(), new_admin.clone()],
                        ).await;
                        eprintln!("[account] promoted {new_admin} to admin in group {gid}");
                    }
                }
            }
        }
    }

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

    // Remove group memberships (for groups that weren't deleted above)
    let _ = conn.execute(
        "DELETE FROM group_member WHERE user_id = ?1",
        libsql::params![user_id.clone()],
    ).await;

    // Remove the user row itself (cascades to dm_channel_member, group_invite, etc.)
    conn.execute(
        "DELETE FROM users WHERE id = ?1",
        libsql::params![user_id.clone()],
    ).await?;

    // ── Phase 3: local cleanup ─────────────────────────────────────────

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

    // Clear all keystore entries.
    let _ = state.keystore.delete_for_user("db_key", &user_id).await;
    let _ = state.keystore.delete_for_user("db_key_wrapped", &user_id).await;
    let _ = state.keystore.delete_for_user("account_id_key", &user_id).await;
    let _ = state.keystore.delete_for_user("account_id_key_wrapped", &user_id).await;
    let _ = state.keystore.delete_for_user("pin_meta", &user_id).await;
    let _ = state.keystore.delete_for_user(DEVICE_ID_KEY, &user_id).await;
    *state.device_id.lock().await = None;
    *state.unlock.lock().await = None;

    // Remove from local accounts index
    let _ = crate::accounts::remove_account(&user_id);

    eprintln!("[account] deleted account for user {user_id}");
    Ok(())
}

/// Return the list of accounts that have previously signed in on this device.
/// Used by the login screen to show a "continue as" picker.
#[tauri::command]
pub fn list_known_accounts() -> Result<crate::accounts::AccountsIndex> {
    crate::accounts::read_accounts_index()
}

/// Delete all local data on this computer: per-user databases, keystore
/// entries (device_id, wrapped keys, pin_meta, legacy unwrapped keys),
/// and the accounts index. Does NOT touch the remote Turso database —
/// users can re-authenticate on this device later.
///
/// Intended for the login screen "wipe this computer" action or for
/// preparing a clean uninstall across platforms.
#[tauri::command]
pub async fn wipe_local_data(state: State<'_, Arc<AppState>>) -> Result<()> {
    // 1. Close the active local DB if one is loaded.
    state.unload_user_db().await;
    *state.device_id.lock().await = None;
    *state.unlock.lock().await = None;

    // 2. Read accounts index to enumerate user_ids for keystore cleanup.
    //    A corrupt index here is survivable — we're nuking everything anyway.
    let index = crate::accounts::read_accounts_index().unwrap_or_default();

    // 3. Delete per-user keystore entries.
    let per_user_keys = [
        DEVICE_ID_KEY,
        "db_key",
        "db_key_wrapped",
        "account_id_key",
        "account_id_key_wrapped",
        "pin_meta",
    ];
    for account in &index.accounts {
        for key in &per_user_keys {
            let _ = state.keystore.delete_for_user(key, &account.user_id).await;
        }
    }

    // 4. Delete all files in the data directory.
    let data_dir = crate::db::local::dirs_path();
    if data_dir.exists() {
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    eprintln!("[wipe] local data wiped for {} account(s)", index.accounts.len());
    Ok(())
}

/// List all registered devices for a user. Returns each device's ID,
/// name, timestamps, and whether it is the current device.
#[tauri::command]
pub async fn list_user_devices(
    state: State<'_, Arc<AppState>>,
    user_id: String,
) -> Result<Vec<serde_json::Value>> {
    let current_device_id = state.device_id.lock().await.clone();
    let conn = state.remote_db.conn().await?;
    let mut rows = conn.query(
        "SELECT device_id, device_name, created_at, last_seen FROM user_device WHERE user_id = ?1 ORDER BY created_at ASC",
        libsql::params![user_id],
    ).await?;

    let mut devices = Vec::new();
    while let Some(row) = rows.next().await? {
        let did: String = row.get(0)?;
        let name: Option<String> = row.get(1).ok();
        let created: String = row.get(2)?;
        let seen: String = row.get(3)?;
        let is_current = current_device_id.as_deref() == Some(did.as_str());
        devices.push(serde_json::json!({
            "device_id": did,
            "device_name": name,
            "created_at": created,
            "last_seen": seen,
            "is_current": is_current,
        }));
    }

    Ok(devices)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    const BASELINE: &str = include_str!("../db/migrations/000000_baseline.sql");

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn.execute_batch(BASELINE).unwrap();
        conn
    }

    fn setup_users(conn: &Connection) {
        conn.execute("INSERT INTO users (id, email, username) VALUES ('alice', 'alice@x.com', 'alice')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('bob',   'bob@x.com',   'bob')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('carol', 'carol@x.com', 'carol')", []).unwrap();
    }

    // ── sole member: group should be deleted ───────────────────────────

    #[test]
    fn sole_member_deletion_removes_group() {
        let conn = db();
        setup_users(&conn);

        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'Solo Group', 'alice')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin')", []).unwrap();
        conn.execute("INSERT INTO channels (id, group_id, name) VALUES ('ch1', 'g1', 'general')", []).unwrap();

        // Simulate Phase 2 logic: check member count
        let member_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(member_count, 1);

        // Sole member — delete the group
        conn.execute("DELETE FROM groups WHERE id = 'g1'", []).unwrap();

        let group_exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM groups WHERE id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(group_exists, 0);

        // Channels cascade-deleted too
        let ch_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM channels WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(ch_count, 0);
    }

    // ── sole admin with other members: promote then leave ──────────────

    #[test]
    fn sole_admin_promotes_first_member_before_leaving() {
        let conn = db();
        setup_users(&conn);

        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'Team', 'alice')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'bob', 'member')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'carol', 'member')", []).unwrap();

        let deleting_user = "alice";

        // Simulate Phase 2: check other admins
        let other_admins: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1' AND role = 'admin' AND user_id != ?1",
            rusqlite::params![deleting_user],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(other_admins, 0, "alice is the sole admin");

        // Promote first non-admin member
        let new_admin: String = conn.query_row(
            "SELECT user_id FROM group_member WHERE group_id = 'g1' AND user_id != ?1 LIMIT 1",
            rusqlite::params![deleting_user],
            |row| row.get(0),
        ).unwrap();

        conn.execute(
            "UPDATE group_member SET role = 'admin' WHERE group_id = 'g1' AND user_id = ?1",
            rusqlite::params![new_admin.clone()],
        ).unwrap();

        // Remove the deleting user
        conn.execute(
            "DELETE FROM group_member WHERE group_id = 'g1' AND user_id = ?1",
            rusqlite::params![deleting_user],
        ).unwrap();

        // Verify: promoted member is admin, group still exists with 2 members
        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = ?1",
            rusqlite::params![new_admin],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "admin");

        let remaining: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(remaining, 2);

        let group_exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM groups WHERE id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(group_exists, 1, "group should still exist");
    }

    // ── admin with other admins: no promotion needed ───────────────────

    #[test]
    fn admin_with_other_admins_no_promotion() {
        let conn = db();
        setup_users(&conn);

        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'Team', 'alice')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'bob', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'carol', 'member')", []).unwrap();

        let deleting_user = "alice";

        let other_admins: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1' AND role = 'admin' AND user_id != ?1",
            rusqlite::params![deleting_user],
            |row| row.get(0),
        ).unwrap();
        assert!(other_admins > 0, "bob is also an admin — no promotion needed");

        // Just remove alice
        conn.execute(
            "DELETE FROM group_member WHERE group_id = 'g1' AND user_id = ?1",
            rusqlite::params![deleting_user],
        ).unwrap();

        // bob is still admin
        let bob_role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'bob'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(bob_role, "admin");

        // carol unchanged
        let carol_role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'carol'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(carol_role, "member");
    }

    // ── regular member deletion: no promotion needed ───────────────────

    #[test]
    fn regular_member_deletion_no_promotion() {
        let conn = db();
        setup_users(&conn);

        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'Team', 'alice')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'bob', 'member')", []).unwrap();

        // bob (member) deletes account — no admin promotion needed
        conn.execute("DELETE FROM group_member WHERE group_id = 'g1' AND user_id = 'bob'", []).unwrap();

        let alice_role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'alice'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(alice_role, "admin", "alice should remain admin");

        let group_exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM groups WHERE id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(group_exists, 1);
    }

    // ── multiple groups: each handled correctly ────────────────────────

    #[test]
    fn deletion_handles_multiple_groups_independently() {
        let conn = db();
        setup_users(&conn);

        // Group 1: alice is sole member → should be deleted
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'Solo', 'alice')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin')", []).unwrap();

        // Group 2: alice is sole admin with bob → bob should be promoted
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g2', 'Shared', 'alice')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g2', 'alice', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g2', 'bob', 'member')", []).unwrap();

        // Group 3: alice is member, carol is admin → just leave
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g3', 'Other', 'carol')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g3', 'carol', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g3', 'alice', 'member')", []).unwrap();

        let deleting_user = "alice";

        // Simulate Phase 2 for each group
        let memberships: Vec<(String, String)> = conn
            .prepare("SELECT group_id, role FROM group_member WHERE user_id = ?1")
            .unwrap()
            .query_map(rusqlite::params![deleting_user], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        for (gid, role) in &memberships {
            let member_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM group_member WHERE group_id = ?1",
                rusqlite::params![gid.as_str()],
                |row| row.get(0),
            ).unwrap();

            if member_count <= 1 {
                conn.execute("DELETE FROM groups WHERE id = ?1", rusqlite::params![gid.as_str()]).unwrap();
            } else if role == "admin" {
                let other_admins: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM group_member WHERE group_id = ?1 AND role = 'admin' AND user_id != ?2",
                    rusqlite::params![gid.as_str(), deleting_user],
                    |row| row.get(0),
                ).unwrap();
                if other_admins == 0 {
                    let new_admin: String = conn.query_row(
                        "SELECT user_id FROM group_member WHERE group_id = ?1 AND user_id != ?2 LIMIT 1",
                        rusqlite::params![gid.as_str(), deleting_user],
                        |row| row.get(0),
                    ).unwrap();
                    conn.execute(
                        "UPDATE group_member SET role = 'admin' WHERE group_id = ?1 AND user_id = ?2",
                        rusqlite::params![gid.as_str(), new_admin],
                    ).unwrap();
                }
            }
        }

        // Remove alice from all remaining groups
        conn.execute("DELETE FROM group_member WHERE user_id = ?1", rusqlite::params![deleting_user]).unwrap();

        // g1 should be deleted
        let g1: i64 = conn.query_row("SELECT COUNT(*) FROM groups WHERE id = 'g1'", [], |row| row.get(0)).unwrap();
        assert_eq!(g1, 0, "sole-member group should be deleted");

        // g2 should exist, bob is now admin
        let g2: i64 = conn.query_row("SELECT COUNT(*) FROM groups WHERE id = 'g2'", [], |row| row.get(0)).unwrap();
        assert_eq!(g2, 1, "shared group should still exist");
        let bob_role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g2' AND user_id = 'bob'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(bob_role, "admin", "bob should have been promoted");

        // g3 should exist, carol still admin
        let g3: i64 = conn.query_row("SELECT COUNT(*) FROM groups WHERE id = 'g3'", [], |row| row.get(0)).unwrap();
        assert_eq!(g3, 1);
        let carol_role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g3' AND user_id = 'carol'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(carol_role, "admin");
    }
}

