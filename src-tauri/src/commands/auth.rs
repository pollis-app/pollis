use serde::{Deserialize, Serialize};
use tauri::State;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Result;
use crate::keystore;
use crate::state::AppState;
use ulid::Ulid;
use crate::signal::identity::{IdentityKey, generate_signed_prekey, generate_one_time_prekeys, load_x25519_secret};
use x25519_dalek::{StaticSecret, PublicKey as X25519PublicKey};
use rand::rngs::OsRng;

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

/// Initialize Signal identity for the given user_id.
/// On first call: generates Ed25519 IK + X25519 IK, SPK, 100 OPKs and uploads to Turso.
/// On subsequent calls: ensures X25519 identity key exists (migration) and returns info.
///
/// NOTE: users.identity_key stores the X25519 public key (not Ed25519).
/// Ed25519 is used only for signing; X25519 is used for Diffie-Hellman key exchange.
#[tauri::command]
pub async fn initialize_identity(
    state: State<'_, Arc<AppState>>,
    user_id: String,
) -> Result<IdentityInfo> {
    let (identity, is_new_ed25519) = match IdentityKey::load().await? {
        Some(ik) => (ik, false),
        None => (IdentityKey::generate_and_store().await?, true),
    };

    // Ensure a dedicated X25519 identity key exists. This is separate from the
    // Ed25519 signing key — it is used exclusively for DH-based key distribution.
    let (x25519_pub, x25519_key_is_new) = match load_x25519_secret("x25519_ik_private").await {
        Ok(secret) => (X25519PublicKey::from(&secret), false),
        Err(_) => {
            let secret = StaticSecret::random_from_rng(OsRng);
            let pub_key = X25519PublicKey::from(&secret);
            keystore::store("x25519_ik_private", secret.as_bytes()).await?;
            eprintln!("[identity] generated new X25519 identity key for user {user_id}");
            (pub_key, true)
        }
    };

    let x25519_pub_bytes: [u8; 32] = *x25519_pub.as_bytes();
    let public_key = hex::encode(x25519_pub_bytes);

    if is_new_ed25519 {
        let (spk_pub, spk_sig) = generate_signed_prekey(1, &identity).await?;
        let opks = generate_one_time_prekeys(1, 100).await?;
        upload_initial_keys(&state, &user_id, &x25519_pub_bytes, 1, &spk_pub, &spk_sig, &opks).await?;
        eprintln!("[identity] uploaded initial keys for new user {user_id}");
        // Stale distribution rows (encrypted with old keys) are now invalid — delete them
        // so senders will redistribute with the new identity key on their next message.
        let conn = state.remote_db.conn().await?;
        let _ = conn.execute(
            "DELETE FROM sender_key_dist WHERE recipient_id = ?1",
            libsql::params![user_id.clone()],
        ).await;
        eprintln!("[identity] cleared stale sender_key_dist rows for {user_id}");
    } else {
        // Existing user: update identity_key to the X25519 public key in case it was
        // previously set to an Ed25519 key (the old broken behaviour).
        let conn = state.remote_db.conn().await?;
        conn.execute(
            "UPDATE users SET identity_key = ?1 WHERE id = ?2",
            libsql::params![public_key.clone(), user_id.clone()],
        ).await?;
        eprintln!("[identity] ensured X25519 identity_key is uploaded for existing user {user_id}");

        // If X25519 key is new (fresh keystore), old distribution rows are stale.
        if x25519_key_is_new {
            let _ = conn.execute(
                "DELETE FROM sender_key_dist WHERE recipient_id = ?1",
                libsql::params![user_id.clone()],
            ).await;
            eprintln!("[identity] cleared stale sender_key_dist rows for {user_id} (new X25519 key)");
        }

        // Ensure an SPK exists in the remote DB. Existing users may have none if they
        // went through the Ed25519→X25519 migration without an SPK upload.
        let mut spk_rows = conn.query(
            "SELECT key_id FROM signed_prekey WHERE user_id = ?1 ORDER BY key_id DESC LIMIT 1",
            libsql::params![user_id.clone()],
        ).await?;
        if spk_rows.next().await?.is_none() {
            let (spk_pub, spk_sig) = generate_signed_prekey(1, &identity).await?;
            conn.execute(
                "INSERT OR REPLACE INTO signed_prekey (user_id, key_id, public_key, signature) VALUES (?1, ?2, ?3, ?4)",
                libsql::params![user_id.clone(), 1i64, hex::encode(&spk_pub), hex::encode(&spk_sig)],
            ).await?;
            eprintln!("[identity] uploaded missing SPK for existing user {user_id}");
        }
    }

    Ok(IdentityInfo { user_id, public_key, is_new: is_new_ed25519 })
}

/// Check whether a Signal identity key exists locally.
#[tauri::command]
pub async fn get_identity() -> Result<Option<IdentityInfo>> {
    match IdentityKey::load().await? {
        Some(identity) => {
            let public_key = hex::encode(identity.public_key_bytes());
            Ok(Some(IdentityInfo {
                user_id: String::new(),
                public_key,
                is_new: false,
            }))
        }
        None => Ok(None),
    }
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
    keystore::store(SESSION_KEY, &session_bytes).await?;

    Ok(profile)
}

/// Load the persisted session from the OS keystore.
/// Verifies the user still exists in Turso — if not, clears the stale session and returns None.
/// Returns None if the user has never signed in or has logged out.
#[tauri::command]
pub async fn get_session(state: State<'_, Arc<AppState>>) -> Result<Option<UserProfile>> {
    let bytes = match keystore::load(SESSION_KEY).await? {
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
                            let _ = keystore::delete(SESSION_KEY).await;
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

    Ok(Some(profile))
}

/// Clear the persisted session (logout). Optionally delete identity keys too.
#[tauri::command]
pub async fn logout(delete_data: bool) -> Result<()> {
    // Always clear the session
    if keystore::load(SESSION_KEY).await?.is_some() {
        keystore::delete(SESSION_KEY).await?;
    }

    if delete_data {
        // Remove identity keys from keystore
        let _ = keystore::delete("identity_key_private").await;
        let _ = keystore::delete("identity_key_public").await;
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

    // Remove sender key distribution rows for this user
    let _ = conn.execute(
        "DELETE FROM sender_key_dist WHERE sender_id = ?1 OR recipient_id = ?1",
        libsql::params![user_id.clone()],
    ).await;

    // Remove encrypted message envelopes sent by this user
    let _ = conn.execute(
        "DELETE FROM message_envelope WHERE sender_id = ?1",
        libsql::params![user_id.clone()],
    ).await;

    // Remove one-time prekeys
    let _ = conn.execute(
        "DELETE FROM one_time_prekey WHERE user_id = ?1",
        libsql::params![user_id.clone()],
    ).await;

    // Remove signed prekeys
    let _ = conn.execute(
        "DELETE FROM signed_prekey WHERE user_id = ?1",
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

    // Clear all keystore entries
    let _ = keystore::delete(SESSION_KEY).await;
    let _ = keystore::delete("identity_key_private").await;
    let _ = keystore::delete("identity_key_public").await;
    let _ = keystore::delete("x25519_ik_private").await;
    let _ = keystore::delete("local_db_key").await;

    // Clear stored signed prekey and one-time prekey entries from keystore
    for i in 1u32..=10 {
        let _ = keystore::delete(&format!("spk_private_{i}")).await;
    }
    for i in 1u32..=110 {
        let _ = keystore::delete(&format!("opk_private_{i}")).await;
    }

    // Delete the local SQLite database file
    {
        let data_dir = {
            if let Ok(dir) = std::env::var("POLLIS_DATA_DIR") {
                std::path::PathBuf::from(dir)
            } else {
                #[cfg(target_os = "macos")]
                {
                    let home = std::env::var("HOME").unwrap_or_default();
                    std::path::PathBuf::from(home)
                        .join("Library/Application Support/com.pollis.app")
                }
                #[cfg(target_os = "linux")]
                {
                    let home = std::env::var("HOME").unwrap_or_default();
                    std::path::PathBuf::from(home).join(".local/share/pollis")
                }
                #[cfg(target_os = "windows")]
                {
                    let appdata = std::env::var("APPDATA").unwrap_or_default();
                    std::path::PathBuf::from(appdata).join("pollis")
                }
            }
        };
        let db_path = data_dir.join("pollis.db");
        if db_path.exists() {
            let _ = std::fs::remove_file(&db_path);
        }
        // Also remove WAL and SHM companion files if present
        let _ = std::fs::remove_file(data_dir.join("pollis.db-wal"));
        let _ = std::fs::remove_file(data_dir.join("pollis.db-shm"));
    }

    eprintln!("[account] deleted account for user {user_id}");
    Ok(())
}

async fn upload_initial_keys(
    state: &AppState,
    user_id: &str,
    identity_key: &[u8; 32],
    spk_id: u32,
    spk_pub: &[u8],
    spk_sig: &[u8],
    opks: &[(u32, Vec<u8>)],
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Update the existing user row with the identity key (user created by verify_otp)
    conn.execute(
        "UPDATE users SET identity_key = ?1 WHERE id = ?2",
        libsql::params![hex::encode(identity_key), user_id],
    ).await?;

    conn.execute(
        "INSERT OR REPLACE INTO signed_prekey (user_id, key_id, public_key, signature) VALUES (?1, ?2, ?3, ?4)",
        libsql::params![user_id, spk_id as i64, hex::encode(spk_pub), hex::encode(spk_sig)],
    ).await?;

    for (id, pub_key) in opks {
        conn.execute(
            "INSERT OR IGNORE INTO one_time_prekey (user_id, key_id, public_key) VALUES (?1, ?2, ?3)",
            libsql::params![user_id, *id as i64, hex::encode(pub_key)],
        ).await?;
    }

    Ok(())
}
