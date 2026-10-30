use serde::{Deserialize, Serialize};

use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;
use ulid::Ulid;

const DEVICE_ID_KEY: &str = "device_id";

/// Per-user keystore slot holding this account's login address (#997).
const LOGIN_EMAIL_KEY: &str = "login_email";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserProfile {
    pub id: String,
    pub email: String,
    pub username: String,
    /// Only populated on first-device signup; cleared before the profile is persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_secret_key: Option<String>,
    /// The account has an identity but this device has no key: route to enrollment.
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
pub async fn initialize_identity(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<IdentityInfo> {
    let device_id = state.device_id.lock().await.clone()
        .ok_or_else(|| anyhow::anyhow!("device_id not set — login incomplete"))?;

    match crate::commands::mls::ensure_mls_key_package(state, &user_id, &device_id).await {
        Ok(()) => crate::diag!("[identity] MLS key package ensured for {user_id} device {device_id}"),
        Err(e) => crate::diag!("[identity] MLS key package error (non-fatal): {e}"),
    }

    // Re-process pending Welcomes: recovers membership after a local DB wipe.
    match crate::commands::mls::poll_mls_welcomes_inner(state, &user_id, &device_id).await {
        Ok(()) => {}
        Err(e) => crate::diag!("[identity] poll_mls_welcomes (non-fatal): {e}"),
    }

    Ok(IdentityInfo { user_id, public_key: String::new(), is_new: false })
}

/// Request an OTP to be sent to the given email address.
/// Generated, stored and emailed server-side by the DS.
pub async fn request_otp(
    state: &Arc<AppState>,
    email: String,
) -> Result<()> {
    crate::diag!(
        "[auth] request_otp: email={} → delivery_url={:?}",
        crate::util::mask_email(&email),
        state.config.pollis_delivery_url
    );
    let resp = crate::commands::mls::ds_post_plain(
        state,
        &pollis_api::otp::RequestOtpBody {
            email,
        },
    )
    .await?;
    let status = resp.status();
    crate::diag!("[auth] request_otp: DS responded HTTP {status}");
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("request-otp failed ({status}): {txt}").into());
    }
    Ok(())
}

/// Verify an OTP code, create or load the user, and complete first-device
/// onboarding.
pub async fn verify_otp(
    state: &Arc<AppState>,
    email: String,
    code: String,
) -> Result<UserProfile> {
    verify_otp_ds(state, email, code).await
}

/// First-device-signup onboarding routed through the Delivery Service.
/// The bootstrap writes are session-gated: they *establish* the device's signing credential.
async fn verify_otp_ds(
    state: &Arc<AppState>,
    email: String,
    code: String,
) -> Result<UserProfile> {
    // A not-yet-enrolled returning device binds the session to its stable id, so it can restart enrollment.
    // Drop claimed-link state, or its tag rides on this enrollment request (#1207).
    *state.device_link_pending.lock().await = None;
    // Nor is it the QR this device may still be showing (#1361).
    *state.device_offer.lock().await = None;
    let candidate_device_id = stable_device_id_for_reenrollment(state, &email)
        .await
        .unwrap_or_else(|| Ulid::new().to_string());

    // 1. verify-otp: unauthenticated; the OTP is the proof.
    crate::diag!(
        "[auth] verify_otp: email={} → delivery_url={:?}",
        crate::util::mask_email(&email),
        state.config.pollis_delivery_url
    );
    let resp = crate::commands::mls::ds_post_plain(
        state,
        &pollis_api::otp::VerifyOtpBody {
            email: email.clone(),
            code,
            device_id: candidate_device_id.clone(),
            // Ignored: `establish-identity` writes the key under its CAS guard.
            account_id_pub: None,
        },
    )
    .await?;
    let status = resp.status();
    crate::diag!("[auth] verify_otp: DS responded HTTP {status}");
    if status.as_u16() == 401 {
        return Err(anyhow::anyhow!("Invalid code. Please check and try again.").into());
    }
    if status.as_u16() == 429 {
        return Err(anyhow::anyhow!(
            "Too many attempts. Please request a new code."
        )
        .into());
    }
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("verify-otp failed ({status}): {txt}").into());
    }
    let v =
        crate::commands::mls::decode_response::<pollis_api::otp::VerifyOtpBody>(resp).await?;

    let user_id = accept_server_user_id(v.user_id)?;
    let username = v.username;
    let has_identity = v.has_identity;
    let session_token = v.session_token;

    // Orphan-detection for a returning device whose server identity no longer
    // matches the local key (a reset elsewhere).
    if has_identity {
        reconcile_account_identity(state, &user_id, v.account_id_pub.as_deref()).await;
    }

    let new_secret_key = if !has_identity {
        crate::diag!("[auth] verify_otp: first-device path (has_identity=false) user_id={user_id}");
        // FORCE the session-bound id: a retry after a partial bootstrap must not reuse the stale one.
        // Safe: `has_identity == false` means no device was ever registered.
        state
            .keystore
            .store_for_user(DEVICE_ID_KEY, &user_id, candidate_device_id.as_bytes())
            .await?;
        *state.device_id.lock().await = Some(candidate_device_id.clone());
        let device_id = candidate_device_id.clone();

        // Pure crypto: keygen + Secret Key + wrap, installed into state.unlock.
        let (secret_key, material) =
            crate::commands::account_identity::generate_account_identity_material(state, &user_id)
                .await
                .map_err(|e| {
                    crate::diag!("[auth] verify_otp: generate_account_identity_material FAILED: {e}");
                    e
                })?;
        crate::diag!("[auth] verify_otp: identity material ready (device_id={device_id}) → establish-identity");

        // 2. establish-identity (session-gated).
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        crate::commands::mls::ds_post_session_ok(
            state,
            &session_token,
            &pollis_api::bootstrap::EstablishIdentityBody {
                account_id_pub: b64.encode(material.account_id_pub),
                salt: b64.encode(material.salt),
                nonce: b64.encode(material.nonce),
                wrapped_key: b64.encode(&material.wrapped_key),
            },
        )
        .await
        .map_err(|e| {
            crate::diag!("[auth] verify_otp: establish-identity FAILED: {e}");
            e
        })?;
        crate::diag!("[auth] verify_otp: establish-identity OK → register-device");

        // 3. register-device (session-gated).
        // OS + form factor only — never the hostname (see `util::device_label`).
        let device_name = crate::util::device_label();
        crate::commands::mls::ds_post_session_ok(
            state,
            &session_token,
            &pollis_api::bootstrap::RegisterDeviceBody {
                device_id: device_id.clone(),
                device_name: Some(device_name),
            },
        )
        .await
        .map_err(|e| {
            crate::diag!("[auth] verify_otp: register-device FAILED: {e}");
            e
        })?;
        crate::diag!("[auth] verify_otp: register-device OK (first-device bootstrap complete)");

        // Stash the session so the pivot — the first publish-device-cert, run by
        // set_pin → ensure_device_cert — can present it.
        *state.bootstrap_session.lock().await = Some(session_token.clone());

        Some(secret_key)
    } else {
        // Existing account: register via the session (no signing credential yet); the key arrives by enrollment.
        bootstrap_existing_identity_device(state, &user_id, &candidate_device_id, &session_token).await?;
        None
    };

    finish_login_profile(state, user_id, email, username, new_secret_key, has_identity).await
}

/// Sign in with the enrollment-only session a claimed QR device link minted (#1207).
pub(crate) async fn sign_in_with_link_session(
    state: &Arc<AppState>,
    v: pollis_api::links::ClaimLinkResponse,
    candidate_device_id: String,
) -> Result<UserProfile> {
    let user_id = accept_server_user_id(v.user_id)?;
    reconcile_account_identity(state, &user_id, v.account_id_pub.as_deref()).await;
    bootstrap_existing_identity_device(state, &user_id, &candidate_device_id, &v.session_token).await?;
    finish_login_profile(state, user_id, v.email, v.username, None, true).await
}

/// The existing-account half of signing a new device in (OTP and QR link, #1207).
pub(crate) async fn bootstrap_existing_identity_device(
    state: &Arc<AppState>,
    user_id: &str,
    candidate_device_id: &str,
    session_token: &str,
) -> Result<()> {
    let user_id = user_id.to_string();
    let session_token = session_token.to_string();
    let stored_device_id = ensure_device_id(state, &user_id, candidate_device_id).await?;

    // Holding the account key, not the id comparison alone, picks the branch (#1099).
    let enrolled = crate::commands::account_identity::has_local_account_identity(
        state, &user_id,
    )
    .await
    .unwrap_or(false);

    // An unenrolled device with a stale id adopts the session's: the pre-enrollment soft reset
    // keeps only the session's device, so a stale id would delete its own row.
    let device_id = if !enrolled && stored_device_id != candidate_device_id {
        state
            .keystore
            .store_for_user(DEVICE_ID_KEY, &user_id, candidate_device_id.as_bytes())
            .await?;
        *state.device_id.lock().await = Some(candidate_device_id.to_string());
        candidate_device_id.to_string()
    } else {
        stored_device_id
    };

    if device_id == candidate_device_id {
        // New device, or one that adopted the session's id: the session authorizes the row.
        // OS + form factor only — never the hostname (see `util::device_label`).
        let device_name = crate::util::device_label();
        crate::commands::mls::ds_post_session_ok(
            state,
            &session_token,
            &pollis_api::bootstrap::RegisterDeviceBody {
                device_id: device_id.clone(),
                device_name: Some(device_name),
            },
        )
        .await?;
        // For the enrollment request; NOT `bootstrap_session` (cert publish stays cert-validity-only).
        *state.enrollment_session.lock().await = Some(session_token.to_string());
    } else {
        // Known device the session can't authorize: `register_device` no-ops its remote write.
        register_device(state, &user_id).await?;
    }
    Ok(())
}

/// Build the signed-in profile and record the account locally — the tail every
/// sign-in path shares (#1207).
pub(crate) async fn finish_login_profile(
    state: &Arc<AppState>,
    user_id: String,
    email: String,
    username: String,
    new_secret_key: Option<String>,
    has_identity: bool,
) -> Result<UserProfile> {
    let enrollment_required = enrollment_required_for(state, &user_id, has_identity).await;

    let profile = UserProfile {
        id: user_id,
        email,
        username,
        new_secret_key,
        enrollment_required,
    };

    crate::accounts::upsert_account(&profile.id, &profile.username, None)?;
    store_login_email(state, &profile.id, &profile.email).await;

    Ok(profile)
}

/// Whether this device must go through the enrollment gate for `user_id`.
/// Needed when the account has an identity and this device has no key, or its key is superseded.
async fn enrollment_required_for(
    state: &Arc<AppState>,
    user_id: &str,
    has_remote_identity: bool,
) -> bool {
    if !has_remote_identity {
        return false;
    }
    let has_local = crate::commands::account_identity::has_local_account_identity(state, user_id)
        .await
        .unwrap_or(false);
    if !has_local {
        return true;
    }
    crate::commands::account_identity::account_identity_superseded(state, user_id)
        .await
        .unwrap_or(false)
}

/// Reconcile this device's local account key against the account identity the
/// server says is current — the signature of a reset performed on another
/// device.
/// A mismatch MARKS the device rather than deleting the key: the value is unauthenticated.
async fn reconcile_account_identity(
    state: &Arc<AppState>,
    user_id: &str,
    account_id_pub: Option<&str>,
) {
    use base64::Engine as _;
    let Some(encoded) = account_id_pub else {
        return;
    };
    let Ok(pub_bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        crate::diag!("[auth] verify-otp returned an undecodable account_id_pub; leaving local key");
        return;
    };

    // Recorded either way: later enrollment / Secret-Key unwraps check against it.
    if let Err(e) = crate::commands::account_identity::remember_published_account_id_pub(
        state, user_id, &pub_bytes,
    )
    .await
    {
        crate::diag!("[auth] could not record the published account_id_pub: {e}");
    }

    let matches = crate::commands::account_identity::has_matching_local_account_identity(
        state, user_id, &pub_bytes,
    )
    .await
    .unwrap_or(false);
    if matches {
        if let Err(e) =
            crate::commands::account_identity::clear_account_identity_superseded(state, user_id)
                .await
        {
            crate::diag!("[auth] could not clear the superseded marker (non-fatal): {e}");
        }
        return;
    }

    crate::diag!(
        "[auth] the server reports a different account identity for {user_id}; \
         marking this device for re-enrollment and KEEPING the local key"
    );
    if let Err(e) = crate::commands::account_identity::mark_account_identity_superseded(
        state, user_id, &pub_bytes,
    )
    .await
    {
        crate::diag!("[auth] could not mark the identity superseded (non-fatal): {e}");
    }
}

/// The chokepoint where a Delivery-Service-minted `user_id` enters the client.
/// The id becomes a directory name, so an absolute one could empty `$HOME`; reject it here.
fn accept_server_user_id(user_id: String) -> Result<String> {
    if !crate::util::is_safe_id(&user_id) {
        // Deliberately not echoing the value: it is attacker-chosen bytes.
        return Err(anyhow::anyhow!(
            "verify-otp returned a malformed user_id ({} bytes); refusing to sign in",
            user_id.len()
        )
        .into());
    }
    Ok(user_id)
}

/// Resolve this device's stable `device_id` for `user_id`, persisting `candidate`
/// in the keystore if none exists yet, and store it in `AppState.device_id`.
async fn ensure_device_id(
    state: &Arc<AppState>,
    user_id: &str,
    candidate: &str,
) -> Result<String> {
    let device_id = match state.keystore.load_for_user(DEVICE_ID_KEY, user_id).await? {
        Some(bytes) => String::from_utf8(bytes)
            .map_err(|e| anyhow::anyhow!("corrupt device_id in keystore: {e}"))?,
        None => {
            state
                .keystore
                .store_for_user(DEVICE_ID_KEY, user_id, candidate.as_bytes())
                .await?;
            candidate.to_string()
        }
    };
    *state.device_id.lock().await = Some(device_id.clone());
    Ok(device_id)
}

/// Read a user's login address out of the per-user keystore (#997).
/// `None` on ANY failure: a keychain hiccup must degrade to "type your email".
async fn login_email_for_user(state: &Arc<AppState>, user_id: &str) -> Option<String> {
    let bytes = state
        .keystore
        .load_for_user(LOGIN_EMAIL_KEY, user_id)
        .await
        .unwrap_or(None)?;
    String::from_utf8(bytes).ok()
}

/// Persist a user's login address in the per-user keystore, next to every other
/// per-user secret. Called by each site that also calls `upsert_account`.
/// Best-effort; returns whether it landed (the upgrade must not erase the file copy first).
async fn store_login_email(state: &Arc<AppState>, user_id: &str, email: &str) -> bool {
    match state
        .keystore
        .store_for_user(LOGIN_EMAIL_KEY, user_id, email.as_bytes())
        .await
    {
        Ok(()) => true,
        Err(e) => {
            crate::diag!("[auth] could not persist the login address for {user_id}: {e}");
            false
        }
    }
}

/// Read the accounts index, first moving any pre-#997 plaintext login address it
/// still carries into the per-user keystore and erasing it from the file.
/// Keystore write first, disk erase second: a failed migration must never lose an address.
async fn read_accounts_index_migrated(
    state: &Arc<AppState>,
) -> Result<crate::accounts::AccountsIndex> {
    let index = crate::accounts::read_accounts_index()?;

    if adopt_legacy_login_emails(state, &index.accounts).await {
        if let Err(e) = crate::accounts::forget_legacy_emails() {
            crate::diag!("[auth] could not erase legacy login addresses from accounts.json: {e}");
        }
    }

    Ok(index)
}

/// Move every pre-#997 address these accounts still carry into the keystore.
/// True only when a legacy address was seen AND every one is now in the keystore.
async fn adopt_legacy_login_emails(
    state: &Arc<AppState>,
    accounts: &[crate::accounts::AccountInfo],
) -> bool {
    let mut saw_legacy = false;
    let mut adopted_all = true;

    for account in accounts {
        let Some(legacy) = account.legacy_email.as_deref() else {
            continue;
        };
        saw_legacy = true;
        // A keystore entry wins: the file's copy is frozen at the upgrade.
        if login_email_for_user(state, &account.user_id).await.is_some() {
            continue;
        }
        if !store_login_email(state, &account.user_id, legacy).await {
            adopted_all = false;
        }
    }

    saw_legacy && adopted_all
}

/// Match an email against the login addresses the keystore holds for `accounts`.
async fn user_id_for_email_among(
    state: &Arc<AppState>,
    accounts: &[crate::accounts::AccountInfo],
    email: &str,
) -> Option<String> {
    for account in accounts {
        let Some(stored) = login_email_for_user(state, &account.user_id).await else {
            continue;
        };
        if stored.eq_ignore_ascii_case(email) {
            return Some(account.user_id.clone());
        }
    }
    None
}

/// Resolve an email address to a `user_id` using only local state.
/// The only lookup available pre-unlock (no signer, no session); `None` = never signed in here.
async fn local_user_id_for_email(state: &Arc<AppState>, email: &str) -> Option<String> {
    let index = read_accounts_index_migrated(state).await.ok()?;
    user_id_for_email_among(state, &index.accounts, email).await
}

/// For a RETURNING, not-yet-enrolled device, its existing `device_id` so the OTP session binds to it.
async fn stable_device_id_for_reenrollment(state: &Arc<AppState>, email: &str) -> Option<String> {
    let user_id = local_user_id_for_email(state, email).await?;

    let enrolled = crate::commands::account_identity::has_local_account_identity(state, &user_id)
        .await
        .unwrap_or(false);
    if enrolled {
        return None;
    }

    let bytes = state
        .keystore
        .load_for_user(DEVICE_ID_KEY, &user_id)
        .await
        .ok()??;
    String::from_utf8(bytes).ok()
}

/// Send an OTP to a new email address as part of the email-change flow.
/// Device-signed; the DS always 200s (anti-enumeration), so uniqueness is decided at verify time.
pub async fn request_email_change_otp(
    state: &Arc<AppState>,
    user_id: String,
    new_email: String,
) -> Result<()> {
    let trimmed = new_email.trim().to_string();
    if trimmed.is_empty() {
        return Err(anyhow::anyhow!("Email is required.").into());
    }

    // `authed` (== this user) is derived from the signature by `ds_post`; the
    // body carries only the new email, so the caller-supplied id is unused.
    let _ = user_id;
    crate::commands::mls::ds_post_ok(
        state,
        &pollis_api::email_change::RequestEmailChangeBody {
            new_email: trimmed,
        },
    )
    .await?;
    Ok(())
}

/// Verify the OTP sent to a new email address and atomically swap `users.email`.
/// `current_code` (to today's address, #1161) is what a borrowed unlocked device can't supply.
pub async fn verify_email_change(
    state: &Arc<AppState>,
    user_id: String,
    new_email: String,
    code: String,
    current_code: String,
) -> Result<()> {
    let trimmed = new_email.trim().to_string();

    let resp = crate::commands::mls::ds_post(
        state,
        &pollis_api::email_change::VerifyEmailChangeBody {
            new_email: trimmed.clone(),
            code,
            current_code: Some(current_code),
        },
    )
    .await?;
    let username: Option<String> = match resp.status().as_u16() {
        200 => {
            // The username rides on the swap (#987), avoiding a racy read-back.
            let pollis_api::email_change::EmailChanged::Ok { username } =
                crate::commands::mls::decode_response::<
                    pollis_api::email_change::VerifyEmailChangeBody,
                >(resp)
                .await?;
            username
        }
        401 => {
            // The DS says which code failed (#1161).
            let txt = resp.text().await.unwrap_or_default();
            if txt.contains("current-address") {
                return Err(anyhow::anyhow!(
                    "The code sent to your current email address didn't match. Check it and try again."
                )
                .into());
            }
            return Err(anyhow::anyhow!("Invalid code. Please check and try again.").into());
        }
        429 => {
            return Err(anyhow::anyhow!("Too many attempts. Please request a new code.").into())
        }
        403 => {
            return Err(anyhow::anyhow!(
                "This change wasn't requested from this account. Request a new code."
            )
            .into())
        }
        409 => {
            return Err(anyhow::anyhow!("That email was just claimed by another account.").into())
        }
        _ => {
            let s = resp.status();
            let txt = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("verify-email-change failed ({s}): {txt}").into());
        }
    };

    // Mirror into the keystore and accounts index for the login picker and pre-unlock lookup.
    if let Some(username) = username {
        if let Err(e) = crate::accounts::upsert_account(&user_id, &username, None) {
            crate::diag!("[auth] email-change accounts.json mirror failed: {e}");
        }
        store_login_email(state, &user_id, &trimmed).await;
    }

    Ok(())
}

/// Dev-only: bypass OTP and log in directly with an email address.
/// Returns an error in release builds so this can never be used in production.
pub async fn dev_login(
    _state: &Arc<AppState>,
    _email: String,
) -> Result<UserProfile> {
    #[cfg(not(debug_assertions))]
    {
        let _ = (_state, _email);
        return Err(crate::error::Error::Other(anyhow::anyhow!("dev_login is not available in release builds")));
    }

    #[cfg(debug_assertions)]
    {
        let profile = dev_login_dispatch(_state, _email).await?;
        crate::diag!("[auth] dev_login: logged in as {}", profile.id);
        Ok(profile)
    }
}

/// This device's stable `device_id`, or `None` if not registered this process (#140).
pub async fn get_device_id(state: &Arc<AppState>) -> Result<Option<String>> {
    Ok(state.device_id.lock().await.clone())
}

pub async fn get_session(state: &Arc<AppState>) -> Result<Option<UserProfile>> {
    // Dev: DEV_EMAIL in .env.development auto-logs in on every startup.
    #[cfg(debug_assertions)]
    if let Ok(dev_email) = std::env::var("DEV_EMAIL") {
        let dev_email = dev_email.trim().to_string();
        if !dev_email.is_empty() {
            crate::diag!("[auth] DEV_EMAIL active — auto-logging in as {}",
                crate::util::mask_email(&dev_email));
            let profile = dev_login_dispatch(state, dev_email).await?;
            return Ok(Some(profile));
        }
    }

    let index = read_accounts_index_migrated(state).await?;
    let account = match index
        .last_active_user
        .and_then(|uid| index.accounts.iter().find(|a| a.user_id == uid).cloned())
    {
        Some(a) => a,
        None => {
            crate::diag!("[session] no last_active_user in accounts index — showing login");
            return Ok(None);
        }
    };

    // Empty on a keystore miss (#997).
    let email = login_email_for_user(state, &account.user_id)
        .await
        .unwrap_or_default();

    let mut profile = UserProfile {
        id: account.user_id.clone(),
        email,
        username: account.username.clone(),
        new_secret_key: None,
        enrollment_required: false,
    };

    // One pre-PIN, unauthenticated probe (#987 §6.5). "exists" fails OPEN (#247): only a
    // confirmed `false` removes the account. "has identity" fails to `false`: no gate offline.
    let probe = match crate::commands::ds_reads::account_probe(state, &profile.id).await {
        Ok(p) => Some(p),
        Err(e) => {
            crate::diag!("[session] account probe failed ({e}); proceeding from accounts.json");
            None
        }
    };
    if probe.as_ref().is_some_and(|p| !p.exists) {
        let _ = crate::accounts::remove_account(&profile.id);
        return Ok(None);
    }

    // Fire-and-forget; a launch failure must not bounce the user to OTP (#247).
    if let Err(e) = register_device(state, &profile.id).await {
        crate::diag!("[session] register_device failed for user {} ({e}); proceeding from accounts.json", profile.id);
    }

    let has_remote_identity = probe.as_ref().is_some_and(|p| p.has_identity);
    profile.enrollment_required =
        enrollment_required_for(state, &profile.id, has_remote_identity).await;

    Ok(Some(profile))
}

/// DEV_EMAIL auto-login entry point.
/// Drives the real DS bootstrap handshake; no Turso-direct fallback (#910).
#[cfg(debug_assertions)]
async fn dev_login_dispatch(state: &Arc<AppState>, email: String) -> Result<UserProfile> {
    // Canonicalize once so every downstream use sees the same address.
    let email = canonical_login_email(&email);

    // No DS, no login (#910): a DS-less client can't write anything anyway.
    if state.config.pollis_delivery_url.is_none() {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "DEV_EMAIL/dev_login needs a Delivery Service: set POLLIS_DELIVERY_URL \
             (see docs/run-it-yourself.md). The DS owns auth and every remote write, \
             so there is nothing a client can do without one."
        )));
    }
    // A DS enrollment failure is an error, not a fallback that looks signed in.
    dev_login_ds(state, email).await.map_err(|e| {
        crate::error::Error::Other(anyhow::anyhow!(
            "DEV_EMAIL DS enrollment failed: {e}. Ensure the Delivery Service is running \
             with DEV_OTP set to the same value as this client (default 000000)."
        ))
    })
}

/// The deterministic dev OTP code. Mirrors the DS's `DEV_OTP` (read server-side in
/// `pollis_delivery::otp`); they must match for `verify-otp` to accept it.
#[cfg(debug_assertions)]
fn dev_otp_code() -> String {
    std::env::var("DEV_OTP")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "000000".to_string())
}

/// DEV_EMAIL auto-login that ENROLLS the device with the DS, binding the OTP session
/// to a returning device's EXISTING `device_id` so `register-device` can create its row.
#[cfg(debug_assertions)]
async fn dev_login_ds(state: &Arc<AppState>, email: String) -> Result<UserProfile> {
    // Resolved from the local accounts index (pre-unlock); a miss takes the first-device path.
    let user_id = match local_user_id_for_email(state, &email).await {
        Some(uid) => uid,
        None => {
            crate::diag!(
                "[auth] DEV_EMAIL: no local account for {}; first-device DS bootstrap",
                crate::util::mask_email(&email)
            );
            request_otp(state, email.clone()).await?;
            return verify_otp(state, email, dev_otp_code()).await;
        }
    };

    // Bind the session to this device's existing id (or a fresh candidate).
    let bound_device_id = match state.keystore.load_for_user(DEVICE_ID_KEY, &user_id).await? {
        Some(bytes) => String::from_utf8(bytes)
            .map_err(|e| anyhow::anyhow!("corrupt device_id in keystore: {e}"))?,
        None => Ulid::new().to_string(),
    };

    request_otp(state, email.clone()).await?;

    let resp = crate::commands::mls::ds_post_plain(
        state,
        &pollis_api::otp::VerifyOtpBody {
            email: email.clone(),
            code: dev_otp_code(),
            device_id: bound_device_id.clone(),
            // Ignored: `establish-identity` writes the key under its CAS guard.
            account_id_pub: None,
        },
    )
    .await?;
    let status = resp.status();
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("dev verify-otp failed ({status}): {txt}").into());
    }
    let v =
        crate::commands::mls::decode_response::<pollis_api::otp::VerifyOtpBody>(resp).await?;
    let username = v.username;
    let has_identity = v.has_identity;
    let session_token = v.session_token;

    let device_id = ensure_device_id(state, &user_id, &bound_device_id).await?;

    // Orphan-detection: a reset elsewhere leaves a stale local key, so the
    // enrollment gate recomputes honestly.
    if has_identity {
        reconcile_account_identity(state, &user_id, v.account_id_pub.as_deref()).await;
    }

    // register-device (session-gated): the row a pre-#419 device is missing.
    // OS + form factor only — never the hostname (see `util::device_label`).
    let device_name = crate::util::device_label();
    crate::commands::mls::ds_post_session_ok(
        state,
        &session_token,
        &pollis_api::bootstrap::RegisterDeviceBody {
            device_id: device_id.clone(),
            device_name: Some(device_name),
        },
    )
    .await?;

    // Stash the session so the cert pivot (`ensure_device_cert`) can present it.
    *state.bootstrap_session.lock().await = Some(session_token);

    let enrollment_required = enrollment_required_for(state, &user_id, has_identity).await;

    crate::accounts::upsert_account(&user_id, &username, None)?;
    store_login_email(state, &user_id, &email).await;

    crate::diag!("[auth] DEV_EMAIL: DS-enrolled device {device_id} for {user_id}");
    Ok(UserProfile {
        id: user_id,
        email,
        username,
        new_secret_key: None,
        enrollment_required,
    })
}

/// The canonical spelling of a login address for the `users` table.
/// Trim only, matching the DS exactly; untrimmed input once created a second account.
#[doc(hidden)]
pub fn canonical_login_email(email: &str) -> String {
    email.trim().to_string()
}

/// Resolve this device's stable `device_id`; the remote row is the DS's.
async fn register_device(state: &Arc<AppState>, user_id: &str) -> Result<String> {
    let device_id = ensure_device_id(state, user_id, &Ulid::new().to_string()).await?;

    // No remote write: callers are pre-unlock; the DS bumps `last_seen` on the first signed call.
    crate::diag!("[auth] device registered: {device_id}");

    Ok(device_id)
}

/// Clear the persisted session (logout). Optionally wipe the per-user DB and identity keys.
pub async fn logout(state: &Arc<AppState>, delete_data: bool) -> Result<()> {
    // A corrupt index has already been renamed aside; log out with an empty one.
    let index = crate::accounts::read_accounts_index().unwrap_or_default();
    let user_id = index.last_active_user;

    // A revoked device is wiped whatever the caller asked for (#1338).
    let revoked = user_id.is_some() && state.revoked_user() == user_id;
    let delete_data = delete_data || revoked;

    // Snapshot, not clear: the device-signed removal below needs it set.
    let device_id = state.device_id.lock().await.clone();

    // Device-signed removal FIRST, while the signer is still live. Best-effort.
    if delete_data {
        if let (Some(uid), Some(did)) = (user_id.as_ref(), device_id.as_ref()) {
            let body = pollis_api::account::LogoutDeviceBody {
                device_id: did.clone(),
                user_id: Some(uid.clone()),
            };
            if let Err(e) = crate::commands::mls::ds_post_ok(state, &body).await {
                crate::diag!("[auth] logout device removal via DS failed (non-fatal): {e}");
            }
        }
    }

    *state.device_id.lock().await = None;
    state.unload_user_db().await;
    *state.unlock.lock().await = None;

    // Every logout drops the notification key escrow (#1599); the next unlock re-creates it.
    if let Some(ref uid) = user_id {
        if let Err(e) = crate::notify::escrow::delete_escrow(state.keystore.as_ref(), uid).await {
            crate::diag!("[auth] logout: notification key escrow not deleted: {e}");
        }
    }

    // Named explicitly: `unload_user_db` already cleared the ambient cache user.
    crate::commands::r2::clear_media_cache(match user_id.as_deref() {
        Some(uid) => crate::commands::r2::CacheScope::User(uid),
        None => crate::commands::r2::CacheScope::Everything,
    });

    // Release attachments the composer had staged but not sent.
    crate::commands::staging::discard_all_staged();

    // Any media URL handed out this session 403s from here on.
    *state.media_server_token.lock().await = None;

    if delete_data {
        if let Some(ref uid) = user_id {
            let _ = state.keystore.delete_for_user("db_key", uid).await;
            let _ = state.keystore.delete_for_user("db_key_wrapped", uid).await;
            let _ = state.keystore.delete_for_user("account_id_key", uid).await;
            let _ = state.keystore.delete_for_user("account_id_key_wrapped", uid).await;
            let _ = state.keystore.delete_for_user("pin_meta", uid).await;
            let _ = state.keystore.delete_for_user(DEVICE_ID_KEY, uid).await;
            let _ = state.keystore.delete_for_user(LOGIN_EMAIL_KEY, uid).await;
            let data_dir = state.local_data_dir();
            if let Ok(db_path) = crate::db::local::user_db_path(&data_dir, uid) {
                let _ = std::fs::remove_file(&db_path);
                let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
                let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
            }
            let _ = crate::accounts::remove_account(uid);
        }
    } else {
        let _ = crate::accounts::clear_last_active_user();
    }

    state.clear_revoked_device();

    Ok(())
}

/// Permanently delete the account: notify conversations, wipe remote via the DS, then local (#103).
pub async fn delete_account(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<()> {
    // Phase 1: wake every conversation so members evict the leaf; one DS read (#987), before the wipe.
    let mine = crate::commands::ds_reads::conversations(state, &user_id).await?;

    {
        for gid in &mine.group_ids {
            // Via the DS: a room-connected-only publish would miss members (#1081).
            if let Err(e) = crate::commands::livekit::publish_to_room_server(
                state,
                gid,
                crate::commands::livekit_signalling::member_left_group_payload(gid),
            ).await {
                crate::diag!("[account] membership_changed for group {gid} failed (non-fatal): {e}");
            }
        }
    }

    {
        for dm_id in &mine.dm_ids {
            // A deleted account is a leave by another name: nobody else has
            // committed the remove, so the wake-up must ask them to (#1081).
            if let Err(e) = crate::commands::livekit::publish_to_room_server(
                state,
                dm_id,
                crate::commands::livekit_signalling::member_left_conversation_payload(dm_id),
            ).await {
                crate::diag!("[account] membership_changed for DM {dm_id} failed (non-fatal): {e}");
            }
        }
    }

    // Phase 2: one DS transaction; a half-deleted account is a corrupt state.
    let body = pollis_api::account::DeleteAccountBody {
        // The DS's no-auth fallback actor (`resolve_actor`); never widens access.
        user_id: Some(user_id.clone()),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Phase 3: local cleanup.
    state.unload_user_db().await;
    {
        let data_dir = state.local_data_dir();
        if let Ok(db_path) = crate::db::local::user_db_path(&data_dir, &user_id) {
            let _ = std::fs::remove_file(&db_path);
            let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
            let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
        }
    }

    let _ = state.keystore.delete_for_user("db_key", &user_id).await;
    let _ = state.keystore.delete_for_user("db_key_wrapped", &user_id).await;
    let _ = state.keystore.delete_for_user("account_id_key", &user_id).await;
    let _ = state.keystore.delete_for_user("account_id_key_wrapped", &user_id).await;
    let _ = state.keystore.delete_for_user("pin_meta", &user_id).await;
    let _ = state.keystore.delete_for_user(DEVICE_ID_KEY, &user_id).await;
    let _ = state.keystore.delete_for_user(LOGIN_EMAIL_KEY, &user_id).await;
    let _ = crate::notify::escrow::delete_escrow(state.keystore.as_ref(), &user_id).await;
    *state.device_id.lock().await = None;
    *state.unlock.lock().await = None;

    // Everything `logout` drops, deletion drops too (#1256).
    crate::commands::r2::clear_media_cache(crate::commands::r2::CacheScope::User(&user_id));
    *state.media_server_token.lock().await = None;
    crate::commands::staging::discard_all_staged();

    let _ = crate::accounts::remove_account(&user_id);

    crate::diag!("[account] deleted account for user {user_id}");
    Ok(())
}

/// Return the list of accounts that have previously signed in on this device.
/// Used by the login screen to show a "continue as" picker.
/// A failed keystore read yields `email: None` for that account only (#997).
pub async fn list_known_accounts(state: &Arc<AppState>) -> Result<crate::accounts::KnownAccounts> {
    let index = read_accounts_index_migrated(state).await?;

    let mut accounts = Vec::with_capacity(index.accounts.len());
    for account in &index.accounts {
        accounts.push(crate::accounts::KnownAccount {
            user_id: account.user_id.clone(),
            username: account.username.clone(),
            email: login_email_for_user(state, &account.user_id).await,
            avatar_url: account.avatar_url.clone(),
            last_seen: account.last_seen.clone(),
        });
    }

    Ok(crate::accounts::KnownAccounts {
        accounts,
        last_active_user: index.last_active_user,
    })
}

/// Delete all local data on this computer: per-user databases, keystore
/// entries (device_id, wrapped keys, pin_meta, legacy unwrapped keys),
/// and the accounts index. Does NOT touch the remote Turso database —
/// users can re-authenticate on this device later.
/// Every per-user keystore slot `wipe_local_data` deletes.
pub(crate) const WIPED_USER_SLOTS: &[&str] = &[
    DEVICE_ID_KEY,
    "db_key",
    "db_key_wrapped",
    "account_id_key",
    "account_id_key_wrapped",
    "pin_meta",
    LOGIN_EMAIL_KEY,
    // The notification key escrow (#1599).
    crate::notify::escrow::ESCROW_SLOT,
];

pub async fn wipe_local_data(state: &Arc<AppState>) -> Result<()> {
    state.unload_user_db().await;
    *state.device_id.lock().await = None;
    *state.unlock.lock().await = None;

    // A corrupt index is survivable; everything goes anyway.
    let index = crate::accounts::read_accounts_index().unwrap_or_default();

    for account in &index.accounts {
        for key in WIPED_USER_SLOTS {
            let _ = state.keystore.delete_for_user(key, &account.user_id).await;
        }
    }

    let data_dir = crate::db::local::dirs_path();
    if data_dir.exists() {
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    // The media cache, which is NOT under that directory on Linux/Windows.
    crate::commands::r2::clear_media_cache(crate::commands::r2::CacheScope::Everything);

    // Release attachments the composer had staged but not sent.
    crate::commands::staging::discard_all_staged();

    crate::diag!("[wipe] local data wiped for {} account(s)", index.accounts.len());
    Ok(())
}

/// List all registered devices for a user. Returns each device's ID,
/// name, timestamps, and whether it is the current device.
pub async fn list_user_devices(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<Vec<serde_json::Value>> {
    let current_device_id = state.device_id.lock().await.clone();
    // Live devices only, oldest first.
    let rows = crate::commands::ds_reads::devices(state, Some(&user_id), Vec::new(), false).await?;

    let devices = rows
        .into_iter()
        .map(|d| {
            let is_current = current_device_id.as_deref() == Some(d.device_id.as_str());
            serde_json::json!({
                "device_id": d.device_id,
                "device_name": d.device_name,
                "created_at": d.created_at,
                "last_seen": d.last_seen,
                "is_current": is_current,
            })
        })
        .collect();

    Ok(devices)
}

/// Revoke one of the calling user's devices; leaf removal continues in the background.
/// The current device cannot revoke itself — `logout(delete_data: true)` is the path for that.
pub async fn revoke_device(
    state: &Arc<AppState>,
    user_id: String,
    device_id: String,
) -> Result<()> {
    let current_device_id = state.device_id.lock().await.clone();
    if current_device_id.as_deref() == Some(device_id.as_str()) {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "cannot revoke the current device — log out instead"
        )));
    }

    // Tombstone, not hard-delete (#372): `leaf_verdict` must tell revoked from lagging.
    let body = pollis_api::account::RevokeDeviceBody {
        device_id: device_id.clone(),
        // The DS's no-auth fallback actor (`resolve_actor`); never widens access.
        user_id: Some(user_id.clone()),
    };
    // The name at revoke time (#987): the audit entry is the last place it can be read.
    let device_name = match crate::commands::mls::ds_post_json(state, &body).await? {
        pollis_api::account::DeviceRevoked::Ok { device_name } => device_name,
        pollis_api::account::DeviceRevoked::NotFound => {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "device not found for this user"
            )))
        }
    };

    // Security log (#947), written before the slow reconcile. Best-effort: the revoke already committed.
    let ev = pollis_api::account::SecurityEventBody {
        kind: "device_revoked".to_string(),
        device_id: Some(device_id.clone()),
        // Legacy "{hostname} ({os})" names are reduced to the OS label; a hostname can embed an IP.
        metadata: device_name.map(|n| format!("name={}", crate::util::public_device_name(&n))),
        // The DS's no-auth fallback actor (`resolve_actor`); never widens access.
        user_id: Some(user_id.clone()),
    };
    if let Err(e) = crate::commands::mls::ds_post_ok(state, &ev).await {
        crate::diag!("[revoke_device] DS security-event failed (non-fatal): {e}");
    }

    // Advisory "signed out" nudge; MLS evicts a device that ignores it. Sent before the removals.
    let payload = crate::commands::livekit_signalling::device_revoked_payload();
    if let Err(e) =
        crate::commands::livekit::publish_to_user_inbox(state, &user_id, payload).await
    {
        crate::diag!("[revoke_device] inbox nudge failed (non-fatal): {e}");
    }

    // Leaf removals run detached: the tombstone already locks the device out, and every
    // member's sweep (`leaf_verdict`) evicts a leaf this task misses.
    spawn_revoked_leaf_removal(state, user_id);

    Ok(())
}

/// Conversations a leaf-removal pass reconciles at once (no ordering between groups).
const REVOKE_RECONCILE_CONCURRENCY: usize = 4;

/// `true` while a pass runs, then whether another revoke landed. Keyed by state address.
static REVOKE_PASSES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<usize, bool>>,
> = std::sync::LazyLock::new(Default::default);

/// Reconcile every conversation of `user_id`, removing revoked leaves. Revokes during a
/// pass coalesce into one more pass.
fn spawn_revoked_leaf_removal(state: &Arc<AppState>, user_id: String) {
    let key = Arc::as_ptr(state) as usize;
    {
        let mut passes = REVOKE_PASSES.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(rerun) = passes.get_mut(&key) {
            *rerun = true;
            return;
        }
        passes.insert(key, false);
    }
    let state = state.clone();
    tokio::spawn(async move {
        loop {
            revoked_leaf_removal_pass(&state, &user_id).await;
            let mut passes = REVOKE_PASSES.lock().unwrap_or_else(|p| p.into_inner());
            if passes.get(&key) == Some(&true) {
                passes.insert(key, false);
                continue;
            }
            passes.remove(&key);
            break;
        }
    });
}

async fn revoked_leaf_removal_pass(state: &Arc<AppState>, user_id: &str) {
    use futures_util::stream::StreamExt as _;
    // One DS read (#987), scoped by the DS to the signer's own memberships.
    let mine = match crate::commands::ds_reads::conversations(state, user_id).await {
        Ok(mine) => mine,
        Err(e) => {
            crate::diag!(
                "[revoke_device] listing conversations failed: {e} — the sweep will evict the leaf"
            );
            return;
        }
    };
    let conversation_ids: Vec<String> = mine.group_ids.into_iter().chain(mine.dm_ids).collect();
    let started = std::time::Instant::now();
    futures_util::stream::iter(&conversation_ids)
        .for_each_concurrent(REVOKE_RECONCILE_CONCURRENCY, |cid| async move {
            if let Err(e) =
                crate::commands::mls::reconcile_group_mls_impl(state, cid, user_id).await
            {
                crate::diag!("[revoke_device] reconcile {cid} failed: {e}");
            }
        })
        .await;
    crate::diag!(
        "[revoke_device] leaf removal pass over {} conversation(s) took {} ms",
        conversation_ids.len(),
        started.elapsed().as_millis()
    );
}

/// The recording half of the `device_revoked` answer, exposed for the flows
/// harness's late-response test (#1338).
#[doc(hidden)]
pub use crate::commands::mls::ds_client::note_revoked_response;

/// Whether THIS process's current device is the one the DS answered
/// `device_revoked` for (#1338). The hosts ask before they act on a
/// `device_revoked` error, so a rejection that arrives late — after a sign-out
/// or a switch of account — never ends a healthy session.
pub async fn is_current_device_revoked(state: &Arc<AppState>) -> Result<bool> {
    let device_id = state.device_id.lock().await.clone();
    let index = crate::accounts::read_accounts_index().unwrap_or_default();
    Ok(match (index.last_active_user, device_id) {
        (Some(user_id), Some(device_id)) => state.is_revoked_device(&user_id, &device_id),
        _ => false,
    })
}

pub async fn is_current_device_registered(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<bool> {
    let device_id = match state.device_id.lock().await.clone() {
        Some(d) => d,
        None => return Ok(true),
    };
    // `include_revoked: false` is the load-bearing `revoked_at IS NULL` (#685): revoke is a tombstone.
    // A revoked device's own read is refused as `device_revoked`: the answer, not an error (#1338).
    let rows = match crate::commands::ds_reads::devices(
        state,
        Some(&user_id),
        vec![device_id.clone()],
        false,
    )
    .await
    {
        Ok(rows) => rows,
        Err(crate::error::Error::DeviceRevoked) => return Ok(false),
        Err(e) => return Err(e),
    };
    Ok(rows.iter().any(|d| d.device_id == device_id))
}

/// The pre-unlock email → `user_id` resolution, after #997 moved the address
/// out of `accounts.json` and into the per-user keystore.
#[cfg(test)]
mod prelock_email_resolution {
    use super::*;
    use crate::accounts::AccountInfo;
    use crate::config::Config;
    use crate::keystore::{InMemoryKeystore, Keystore};

    fn locked_state() -> Arc<AppState> {
        let keystore: Arc<dyn Keystore> = Arc::new(InMemoryKeystore::new());
        Arc::new(AppState::new_with_parts(
            Config::for_test().expect("test config"),
            keystore,
        ))
    }

    fn account(user_id: &str, username: &str) -> AccountInfo {
        AccountInfo {
            user_id: user_id.to_string(),
            username: username.to_string(),
            avatar_url: None,
            last_seen: "2026-08-01T10:00:00+00:00".to_string(),
            legacy_email: None,
        }
    }

    /// An entry as a pre-#997 `accounts.json` deserializes to it.
    fn legacy_account(user_id: &str, username: &str, email: &str) -> AccountInfo {
        AccountInfo {
            legacy_email: Some(email.to_string()),
            ..account(user_id, username)
        }
    }

    async fn assert_fully_locked(state: &Arc<AppState>) {
        assert!(state.local_db.lock().await.is_none(), "local DB must be closed");
        assert!(state.unlock.lock().await.is_none(), "no unlocked session");
        assert!(state.device_id.lock().await.is_none(), "no device id yet");
    }

    /// The returning-device path (#419): a machine that has signed into this
    /// address before resolves its `user_id` with nothing open.
    #[tokio::test]
    async fn a_returning_address_resolves_with_the_database_closed() {
        let state = locked_state();
        let accounts = vec![account("01USER_A", "alice"), account("01USER_B", "bob")];
        store_login_email(&state, "01USER_A", "alice@example.com").await;
        store_login_email(&state, "01USER_B", "bob@example.com").await;

        assert_fully_locked(&state).await;

        assert_eq!(
            user_id_for_email_among(&state, &accounts, "bob@example.com").await,
            Some("01USER_B".to_string())
        );
        assert_eq!(
            user_id_for_email_among(&state, &accounts, "alice@example.com").await,
            Some("01USER_A".to_string())
        );
    }

    /// Real addresses (not masked ones) can't collide on a shared domain.
    #[tokio::test]
    async fn matching_is_case_insensitive_and_never_collides_on_the_domain() {
        let state = locked_state();
        let accounts = vec![account("01USER_A", "alice"), account("01USER_B", "bob")];
        store_login_email(&state, "01USER_A", "Alice@Example.com").await;
        store_login_email(&state, "01USER_B", "bob@example.com").await;

        assert_eq!(
            user_id_for_email_among(&state, &accounts, "ALICE@EXAMPLE.COM").await,
            Some("01USER_A".to_string())
        );
        assert_eq!(
            user_id_for_email_among(&state, &accounts, "BOB@example.com").await,
            Some("01USER_B".to_string())
        );
    }

    /// An address this machine has never signed into resolves to `None` — the
    /// signal both callers read as "fresh device, take the first-device path".
    #[tokio::test]
    async fn an_unknown_address_does_not_resolve() {
        let state = locked_state();
        let accounts = vec![account("01USER_A", "alice")];
        store_login_email(&state, "01USER_A", "alice@example.com").await;

        assert_eq!(
            user_id_for_email_among(&state, &accounts, "stranger@example.com").await,
            None
        );
    }

    /// An indexed account with no keystore entry is skipped, not matched.
    #[tokio::test]
    async fn an_account_without_a_stored_address_is_skipped_not_matched() {
        let state = locked_state();
        let accounts = vec![account("01LEGACY", "legacy"), account("01USER_B", "bob")];
        store_login_email(&state, "01USER_B", "bob@example.com").await;

        assert_eq!(
            user_id_for_email_among(&state, &accounts, "legacy@example.com").await,
            None
        );
        assert_eq!(
            user_id_for_email_among(&state, &accounts, "bob@example.com").await,
            Some("01USER_B".to_string())
        );
    }

    /// `AccountInfo` has no field that can hold the address.
    #[tokio::test]
    async fn the_address_is_readable_from_the_keystore_and_absent_from_the_index() {
        let state = locked_state();
        let entry = account("01USER_A", "alice");
        store_login_email(&state, "01USER_A", "alice@example.com").await;

        assert_eq!(
            login_email_for_user(&state, "01USER_A").await,
            Some("alice@example.com".to_string())
        );

        let serialized = serde_json::to_string(&entry).expect("serialize");
        assert!(
            !serialized.contains("alice@example.com") && !serialized.contains("email"),
            "the index entry must not be able to carry an address: {serialized}"
        );
    }

    /// The upgrade path: a pre-#997 address is adopted, then resolves.
    #[tokio::test]
    async fn a_legacy_address_is_adopted_into_the_keystore_and_then_resolves() {
        let state = locked_state();
        let accounts = vec![
            legacy_account("01USER_A", "alice", "alice@example.com"),
            legacy_account("01USER_B", "bob", "bob@example.com"),
        ];

        // Before the migration the keystore knows nothing, so nothing resolves.
        assert_eq!(login_email_for_user(&state, "01USER_A").await, None);
        assert_eq!(
            user_id_for_email_among(&state, &accounts, "alice@example.com").await,
            None
        );

        assert!(
            adopt_legacy_login_emails(&state, &accounts).await,
            "every legacy address was adopted, so the file may be rewritten"
        );

        assert_fully_locked(&state).await;
        assert_eq!(
            user_id_for_email_among(&state, &accounts, "alice@example.com").await,
            Some("01USER_A".to_string())
        );
        assert_eq!(
            user_id_for_email_among(&state, &accounts, "bob@example.com").await,
            Some("01USER_B".to_string())
        );
    }

    /// An index with nothing legacy in it must not authorize a rewrite.
    #[tokio::test]
    async fn an_index_with_no_legacy_address_authorizes_no_rewrite() {
        let state = locked_state();
        let accounts = vec![account("01USER_A", "alice")];

        assert!(!adopt_legacy_login_emails(&state, &accounts).await);
    }

    /// A stale file address never overwrites a newer keystore one.
    #[tokio::test]
    async fn a_stale_file_address_never_overwrites_the_keystore() {
        let state = locked_state();
        let accounts = vec![legacy_account("01USER_A", "alice", "old@example.com")];
        store_login_email(&state, "01USER_A", "new@example.com").await;

        assert!(adopt_legacy_login_emails(&state, &accounts).await);

        assert_eq!(
            login_email_for_user(&state, "01USER_A").await,
            Some("new@example.com".to_string())
        );
    }

    /// Deleting the account clears the address with everything else, so a wiped
    /// account leaves nothing behind to resolve.
    #[tokio::test]
    async fn deleting_the_stored_address_unresolves_the_account() {
        let state = locked_state();
        let accounts = vec![account("01USER_A", "alice")];
        store_login_email(&state, "01USER_A", "alice@example.com").await;

        state
            .keystore
            .delete_for_user(LOGIN_EMAIL_KEY, "01USER_A")
            .await
            .expect("delete");

        assert_eq!(login_email_for_user(&state, "01USER_A").await, None);
        assert_eq!(
            user_id_for_email_among(&state, &accounts, "alice@example.com").await,
            None
        );
    }
}

/// A DS-chosen `user_id` is a directory name downstream; a malicious one stops at the response.
#[cfg(test)]
mod server_user_id_is_not_a_path {
    use super::*;
    use crate::config::Config;
    use crate::keystore::{InMemoryKeystore, Keystore};
    use pollis_api::DsRequest as _;

    const HOSTILE_ID: &str = "/home/alice";

    /// A DS answering verify-otp with any `user_id`; past the check, the next post 404s.
    async fn hostile_ds(user_id: &'static str) -> String {
        use axum::{routing::post, Json, Router};
        let app = Router::new().route(
            pollis_api::otp::VerifyOtpBody::PATH,
            post(move || async move {
                Json(pollis_api::otp::VerifyOtpResponse {
                    user_id: user_id.to_string(),
                    username: "alice".to_string(),
                    is_new_account: true,
                    has_identity: false,
                    session_token: "otp-session".to_string(),
                    session_expires_at: i64::MAX,
                    account_id_pub: None,
                })
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve hostile DS");
        });
        format!("http://127.0.0.1:{port}")
    }

    async fn state_against(ds_url: String) -> (Arc<AppState>, Arc<dyn Keystore>) {
        let mut config = Config::for_test().expect("test config");
        config.pollis_delivery_url = Some(ds_url);
        let keystore: Arc<dyn Keystore> = Arc::new(InMemoryKeystore::new());
        (
            Arc::new(AppState::new_with_parts(config, keystore.clone())),
            keystore,
        )
    }

    /// `{"user_id": "/home/alice"}` ends the sign-in with nothing persisted.
    #[tokio::test]
    async fn a_path_like_user_id_ends_the_sign_in_before_anything_is_persisted() {
        let (state, keystore) = state_against(hostile_ds(HOSTILE_ID).await).await;

        let err = verify_otp(&state, "alice@example.com".to_string(), "123456".to_string())
            .await
            .expect_err("a path-like user_id must be refused");
        assert!(
            err.to_string().contains("malformed user_id"),
            "refused for the wrong reason: {err}"
        );
        assert!(
            !err.to_string().contains(HOSTILE_ID),
            "the attacker-chosen id must not be echoed into the error: {err}"
        );

        // Nothing was persisted under the hostile id.
        assert!(keystore
            .load_for_user(DEVICE_ID_KEY, HOSTILE_ID)
            .await
            .expect("keystore read")
            .is_none());
        assert!(state.device_id.lock().await.is_none());
        let index = crate::accounts::read_accounts_index().unwrap_or_default();
        assert!(!index.accounts.iter().any(|a| a.user_id == HOSTILE_ID));
        assert_ne!(index.last_active_user.as_deref(), Some(HOSTILE_ID));
    }

    /// Positive control: a well-formed id gets past the check (then fails on establish-identity).
    #[tokio::test]
    async fn a_well_formed_user_id_gets_past_the_check() {
        let (state, _keystore) = state_against(hostile_ds("01JWELLFORMEDULIDFORTEST00").await).await;

        let err = verify_otp(&state, "alice@example.com".to_string(), "123456".to_string())
            .await
            .expect_err("the fake DS serves no establish-identity, so bootstrap fails");
        assert!(
            !err.to_string().contains("malformed user_id"),
            "a plain id was refused as malformed: {err}"
        );
    }

    #[test]
    fn the_chokepoint_accepts_exactly_what_is_safe_id_accepts() {
        assert_eq!(
            accept_server_user_id("01JCACHEUSERLIFECYCLE0000".to_string()).ok().as_deref(),
            Some("01JCACHEUSERLIFECYCLE0000")
        );
        for bad in ["", "/home/alice", "..", "../../..", "a/b", "a.b"] {
            assert!(
                accept_server_user_id(bad.to_string()).is_err(),
                "{bad:?} must be refused"
            );
        }
    }
}

#[cfg(test)]
mod canonical_email_tests {
    use super::*;

    #[test]
    fn canonicalization_trims_but_never_lowercases() {
        // Lowercasing would silently merge two rows the DS keeps apart: the DS
        // lowercases only the OTP store key, never `users.email`.
        assert_eq!(canonical_login_email("  a@x.com \t"), "a@x.com");
        assert_eq!(canonical_login_email("Alice@X.com"), "Alice@X.com");
    }
}

#[cfg(test)]
mod identity_reconciliation_tests {
    use super::*;
    use crate::commands::account_identity as ident;
    use base64::Engine as _;
    use ml_dsa::Keypair as _;

    const ACCOUNT_ID_KEY_SLOT: &str = "account_id_key";

    fn test_state() -> Arc<AppState> {
        Arc::new(AppState::new_with_parts(
            crate::config::Config::for_test().expect("test config"),
            Arc::new(crate::keystore::InMemoryKeystore::new()),
        ))
    }

    /// `(seed, base64 public half)` for a real account identity.
    fn identity(seed_byte: u8) -> ([u8; 32], String) {
        let seed = [seed_byte; 32];
        let encoded = base64::engine::general_purpose::STANDARD.encode(
            ml_dsa::SigningKey::<ml_dsa::MlDsa44>::from_seed(&seed.into())
                .verifying_key()
                .encode(),
        );
        (seed, encoded)
    }

    async fn seed_local_key(state: &Arc<AppState>, user_id: &str, seed: &[u8]) {
        state
            .keystore
            .store_for_user(ACCOUNT_ID_KEY_SLOT, user_id, seed)
            .await
            .unwrap();
    }

    /// M2: an unauthenticated forged `account_id_pub` must not delete the local key.
    #[tokio::test]
    async fn a_forged_account_id_pub_does_not_destroy_the_local_key() {
        let state = test_state();
        let (mine, _) = identity(11);
        let (_, forged_pub) = identity(99);
        seed_local_key(&state, "u1", &mine).await;

        reconcile_account_identity(&state, "u1", Some(forged_pub.as_str())).await;

        assert_eq!(
            state
                .keystore
                .load_for_user(ACCOUNT_ID_KEY_SLOT, "u1")
                .await
                .unwrap()
                .as_deref(),
            Some(&mine[..]),
            "an unauthenticated server claim must not delete the account identity key"
        );
        assert!(
            ident::account_identity_superseded(&state, "u1")
                .await
                .unwrap(),
            "but the device must be marked, so the user is still sent to re-enroll"
        );
        assert!(
            enrollment_required_for(&state, "u1", true).await,
            "the gate the wipe used to produce indirectly must still appear"
        );
    }

    /// The matching case leaves the device alone and clears any stale marker —
    /// a server that retracts the claim heals the device.
    #[tokio::test]
    async fn a_matching_account_id_pub_clears_the_marker() {
        let state = test_state();
        let (mine, mine_pub) = identity(11);
        let (_, forged_pub) = identity(99);
        seed_local_key(&state, "u1", &mine).await;

        reconcile_account_identity(&state, "u1", Some(forged_pub.as_str())).await;
        assert!(ident::account_identity_superseded(&state, "u1")
            .await
            .unwrap());

        reconcile_account_identity(&state, "u1", Some(mine_pub.as_str())).await;
        assert!(
            !ident::account_identity_superseded(&state, "u1")
                .await
                .unwrap(),
            "the marker must be reversible"
        );
        assert!(!enrollment_required_for(&state, "u1", true).await);
    }

    /// An absent or undecodable value changes nothing at all.
    #[tokio::test]
    async fn an_absent_or_undecodable_value_changes_nothing() {
        let state = test_state();
        let (mine, _) = identity(11);
        seed_local_key(&state, "u1", &mine).await;

        reconcile_account_identity(&state, "u1", None).await;
        reconcile_account_identity(&state, "u1", Some("!!! not base64 !!!")).await;

        assert_eq!(
            state
                .keystore
                .load_for_user(ACCOUNT_ID_KEY_SLOT, "u1")
                .await
                .unwrap()
                .as_deref(),
            Some(&mine[..])
        );
        assert!(!ident::account_identity_superseded(&state, "u1")
            .await
            .unwrap());
        assert!(!enrollment_required_for(&state, "u1", true).await);
    }

    /// Reconciliation is also where the device learns the value a later
    /// enrollment unwrap is checked against (M1).
    #[tokio::test]
    async fn reconciliation_records_the_published_identity_key() {
        let state = test_state();
        let (_, mine_pub) = identity(11);
        reconcile_account_identity(&state, "u1", Some(mine_pub.as_str())).await;

        let recorded = ident::published_account_id_pub(&state, "u1")
            .await
            .unwrap()
            .expect("verify-otp's value must be recorded");
        assert_eq!(
            base64::engine::general_purpose::STANDARD.encode(&recorded),
            mine_pub
        );
    }

    /// An account with no identity never needs the gate, whatever the local
    /// state is.
    #[tokio::test]
    async fn no_remote_identity_means_no_enrollment_gate() {
        let state = test_state();
        assert!(!enrollment_required_for(&state, "u1", false).await);
    }
}
