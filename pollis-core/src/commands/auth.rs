use serde::{Deserialize, Serialize};

use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;
use ulid::Ulid;

const DEVICE_ID_KEY: &str = "device_id";

/// Per-user keystore slot holding this account's login address (#997), kept out
/// of the unencrypted `accounts.json` because it maps an account id to a person.
/// Every reader runs where the keystore is reachable.
const LOGIN_EMAIL_KEY: &str = "login_email";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserProfile {
    pub id: String,
    pub email: String,
    pub username: String,
    /// Only set on first-device signup, and cleared before the profile is persisted,
    /// so the Secret Key never reaches disk. The frontend shows it once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_secret_key: Option<String>,
    /// The server has an `account_id_pub` but this device holds no account key (or a
    /// superseded one): route to the enrollment gate first.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub enrollment_required: bool,
}

#[derive(Debug, Serialize)]
pub struct IdentityInfo {
    pub user_id: String,
    pub public_key: String,
    pub is_new: bool,
}

/// Ensure this user has MLS credentials and a published KeyPackage, so they are
/// invitable. Called after login.
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

    // Re-process pending Welcomes: a no-op normally, but restores memberships after
    // the local DB was wiped (`load_user_db` re-arms delivery then).
    match crate::commands::mls::poll_mls_welcomes_inner(state, &user_id, &device_id).await {
        Ok(()) => {}
        Err(e) => crate::diag!("[identity] poll_mls_welcomes (non-fatal): {e}"),
    }

    Ok(IdentityInfo { user_id, public_key: String::new(), is_new: false })
}

/// Request an OTP for `email`. Generated, stored and sent by the DS
/// (`POST /v1/auth/request-otp`); see `docs/otp-server-bootstrap-design.md`.
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

/// Verify an OTP, create or load the user, and complete first-device onboarding,
/// all via the DS behind an OTP-session token ([`verify_otp_ds`]).
pub async fn verify_otp(
    state: &Arc<AppState>,
    email: String,
    code: String,
) -> Result<UserProfile> {
    verify_otp_ds(state, email, code).await
}

/// Sign-in via the DS. `verify-otp` validates the code, creates/loads the
/// account and mints an OTP-session token, which authorizes the bootstrap writes
/// that establish the device's signing credential (so can't be device-signed):
/// `establish-identity` (public + Secret-Key-wrapped material only),
/// `register-device`, and later the first `publish-device-cert` via
/// `bootstrap_session`.
///
/// A returning account registers this device and gets the account key by
/// sibling approval or Secret-Key recovery.
async fn verify_otp_ds(
    state: &Arc<AppState>,
    email: String,
    code: String,
) -> Result<UserProfile> {
    // Bind the session to the device id. A returning, not-yet-enrolled device
    // reuses its stable id: a fresh candidate would never match the keystore, so it
    // could never restart an unfinished enrollment. `register-device` is an
    // idempotent upsert. An email sign-in is never a link sign-in, so drop any
    // leftover QR state (#1207) or its tag would ride on this request.
    *state.device_link_pending.lock().await = None;
    // Nor the QR this device may still be showing (#1361).
    *state.device_offer.lock().await = None;
    let candidate_device_id = stable_device_id_for_reenrollment(state, &email)
        .await
        .unwrap_or_else(|| Ulid::new().to_string());

    // Unauthenticated: the OTP is the proof.
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
            // `establish-identity` writes the identity key, under its CAS guard.
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
    // Typed (#922), so a renamed field fails to build instead of breaking sign-in.
    let v =
        crate::commands::mls::decode_response::<pollis_api::otp::VerifyOtpBody>(resp).await?;

    let user_id = accept_server_user_id(v.user_id)?;
    let username = v.username;
    let has_identity = v.has_identity;
    let session_token = v.session_token;

    // A returning device whose server identity no longer matches (reset elsewhere).
    if has_identity {
        reconcile_account_identity(state, &user_id, v.account_id_pub.as_deref()).await;
    }

    let new_secret_key = if !has_identity {
        crate::diag!("[auth] verify_otp: first-device path (has_identity=false) user_id={user_id}");
        // Force this device's id to the session-bound candidate. On a retry after a
        // partial bootstrap the keystore holds the aborted attempt's id, which the new
        // session doesn't authorize, wedging every retry. With no identity yet no device
        // was ever registered, so overwriting is safe; this makes signup idempotent.
        state
            .keystore
            .store_for_user(DEVICE_ID_KEY, &user_id, candidate_device_id.as_bytes())
            .await?;
        *state.device_id.lock().await = Some(candidate_device_id.clone());
        let device_id = candidate_device_id.clone();

        let (secret_key, material) =
            crate::commands::account_identity::generate_account_identity_material(state, &user_id)
                .await
                .map_err(|e| {
                    crate::diag!("[auth] verify_otp: generate_account_identity_material FAILED: {e}");
                    e
                })?;
        crate::diag!("[auth] verify_otp: identity material ready (device_id={device_id}) → establish-identity");

        // The DS does the CAS update, `account_key_log` v1 and `account_recovery` insert.
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

        // OS and form factor only, never the hostname (see `util::device_label`).
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

        // For the first `publish-device-cert` (`set_pin` → `ensure_device_cert`).
        *state.bootstrap_session.lock().await = Some(session_token.clone());

        Some(secret_key)
    } else {
        // An existing account: register this device under the session (it can't
        // device-sign yet). The key arrives by sibling approval or Secret-Key recovery;
        // the first cert publish is then gated by cert validity alone, since approval
        // can outlast the session (`docs/otp-server-bootstrap-design.md` §5).
        bootstrap_existing_identity_device(state, &user_id, &candidate_device_id, &session_token).await?;
        None
    };

    finish_login_profile(state, user_id, email, username, new_secret_key, has_identity).await
}

/// Sign in with the enrollment-only session a claimed QR link minted (#1207): the
/// account has an identity, so this is verify-otp's existing-account path.
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

/// The existing-account half of signing a device in, shared by email OTP and the
/// QR link (#1207): resolve the device id, register it under the session, and
/// hold the session for the enrollment request that follows.
pub(crate) async fn bootstrap_existing_identity_device(
    state: &Arc<AppState>,
    user_id: &str,
    candidate_device_id: &str,
    session_token: &str,
) -> Result<()> {
    let user_id = user_id.to_string();
    let session_token = session_token.to_string();
    let stored_device_id = ensure_device_id(state, &user_id, candidate_device_id).await?;

    // Whether this device holds the account key decides the branch below (#1099).
    let enrolled = crate::commands::account_identity::has_local_account_identity(
        state, &user_id,
    )
    .await
    .unwrap_or(false);

    // A not-yet-enrolled device with a stale local id adopts the session's
    // candidate (e.g. after an email change elsewhere, verify-otp couldn't find the
    // stored id). Every following session-gated write is bound to the candidate,
    // and the pre-enrollment soft reset keeps exactly the session's device: keeping
    // the stale id would delete this device's own row. Nothing is lost, since an
    // unenrolled row has no cert or MLS state. An enrolled device also mismatches
    // by design and keeps its stored id.
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
        // New device, or one that just adopted the session's id: the session authorizes
        // exactly this id. OS and form factor only, never the hostname.
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
        // For the session-gated enrollment request next. Not `bootstrap_session`: the
        // cert publish must stay on the cert-validity-alone path.
        *state.enrollment_session.lock().await = Some(session_token.to_string());
    } else {
        // A device already registered under an older id: the session can't authorize
        // it and the local DB is closed, so `register_device` skips the remote write
        // (the row exists with a cert) but still sets `state.device_id`.
        register_device(state, &user_id).await?;
    }
    Ok(())
}

/// Build the signed-in profile and record the account locally (#1207).
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

/// Whether this device must go through the enrollment gate: the account has an
/// identity and this device holds no key, or holds one the server says is
/// superseded ([`reconcile_account_identity`]). A keystore error reads as "not
/// superseded" rather than trapping a working device.
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

/// Reconcile the local account key against the identity verify-otp reported
/// (#987).
///
/// A mismatch marks the device instead of deleting the key: the value is
/// unauthenticated (local DB closed, no TOFU pin, no transparency check), and
/// deleting on it let one hostile DS response destroy a user's only copy. The
/// marker drives `enrollment_required`; `adopt_recovered_account_identity` clears
/// both on re-enrollment, and a match clears the marker. Absent or undecodable
/// values change nothing.
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

    // Recorded either way: later unwraps on this device are checked against it.
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

/// The chokepoint where a DS-minted `user_id` enters the client.
///
/// Downstream it becomes a directory name (media cache, `pollis_<id>.db`,
/// keystore slots), and `Path::join` with an absolute id replaces the root: a DS
/// answering `"/home/alice"` used to have set-PIN empty the home directory. A bad
/// id ends the sign-in here; `r2` and `db::local` re-check at the join.
fn accept_server_user_id(user_id: String) -> Result<String> {
    if !crate::util::is_safe_id(&user_id) {
        // Not echoed: attacker-chosen bytes.
        return Err(anyhow::anyhow!(
            "verify-otp returned a malformed user_id ({} bytes); refusing to sign in",
            user_id.len()
        )
        .into());
    }
    Ok(user_id)
}

/// Resolve this device's stable `device_id` for `user_id`, persisting `candidate`
/// if none exists, and set `AppState.device_id`. No remote write.
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

/// Read a user's login address from the keystore (#997). `None` on any failure:
/// a transient keychain error must degrade to "type your email", never fail the
/// call.
async fn login_email_for_user(state: &Arc<AppState>, user_id: &str) -> Option<String> {
    let bytes = state
        .keystore
        .load_for_user(LOGIN_EMAIL_KEY, user_id)
        .await
        .unwrap_or(None)?;
    String::from_utf8(bytes).ok()
}

/// Persist a user's login address in the keystore. Best-effort, like
/// [`login_email_for_user`]. Returns whether it landed, so the upgrade path
/// doesn't erase the on-disk copy first.
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

/// Read the accounts index, first moving any pre-#997 login address it still
/// carries into the keystore and erasing it from the file.
///
/// Without this, upgraded installs would lose pre-unlock email-to-`user_id`
/// resolution and the "continue as" chip until the next OTP. Keystore write
/// first, disk erase second, and only if every account's write succeeded, so a
/// failed migration never loses an address.
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

/// Move every pre-#997 address into the keystore. True only when at least one
/// was seen and all of them landed. Split at the index read so it's testable
/// with a per-`AppState` keystore.
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
        // A keystore entry wins: it has been kept current since the upgrade.
        if login_email_for_user(state, &account.user_id).await.is_some() {
            continue;
        }
        if !store_login_email(state, &account.user_id, legacy).await {
            adopted_all = false;
        }
    }

    saw_legacy && adopted_all
}

/// Match an email against the keystore's login addresses for `accounts`. Split
/// at the index read (process-global) so the rule is testable with a hermetic
/// keystore. N is a handful, so a linear scan is fine.
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

/// Resolve an email to a `user_id` from local state only: pre-unlock there is no
/// credential for a DS read (#987). `None` means this machine never signed into
/// that address, which is when callers want the fresh-device path.
async fn local_user_id_for_email(state: &Arc<AppState>, email: &str) -> Option<String> {
    let index = read_accounts_index_migrated(state).await.ok()?;
    user_id_for_email_among(state, &index.accounts, email).await
}

/// For a returning, not-yet-enrolled device, its existing stable `device_id`, so
/// `verify_otp_ds` can bind the OTP session to it. `None` (use a fresh id) for a
/// new device, an enrolled one, or no stored id. Local and keystore reads only,
/// so valid pre-unlock.
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

/// Send an OTP to a new email address for an email change. Device-signed
/// `POST /v1/auth/request-email-change-otp`; the DS records the request and sends
/// the OTP. It always answers 200 (anti-enumeration), so uniqueness is decided at
/// verify time.
pub async fn request_email_change_otp(
    state: &Arc<AppState>,
    user_id: String,
    new_email: String,
) -> Result<()> {
    let trimmed = new_email.trim().to_string();
    if trimmed.is_empty() {
        return Err(anyhow::anyhow!("Email is required.").into());
    }

    // The DS takes the user from the signature; the body carries only the new email.
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

/// Verify the new address's OTP and swap `users.email`. Three proofs: the
/// device signature (this account), `code` (controls the new mailbox) and
/// `current_code` (controls the current one, #1161, out of reach of someone with
/// a borrowed unlocked device). The DS binds the user from the signature; the
/// client then mirrors the address locally.
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
            // The username rides on the swap (#987) for the accounts-index mirror, avoiding
            // a read-back that could observe an unrelated rename.
            let pollis_api::email_change::EmailChanged::Ok { username } =
                crate::commands::mls::decode_response::<
                    pollis_api::email_change::VerifyEmailChangeBody,
                >(resp)
                .await?;
            username
        }
        401 => {
            // The DS says which of the two codes failed (#1161).
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

    // Mirror the new address locally so the "continue as" picker and pre-unlock
    // `local_user_id_for_email` follow the account.
    if let Some(username) = username {
        if let Err(e) = crate::accounts::upsert_account(&user_id, &username, None) {
            crate::diag!("[auth] email-change accounts.json mirror failed: {e}");
        }
        store_login_email(state, &user_id, &trimmed).await;
    }

    Ok(())
}

/// Dev-only: log in directly with an email, skipping OTP. An error in release
/// builds.
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

/// This device's stable `device_id`, or `None` if not registered yet this
/// process. The frontend uses it to build its own voice identity
/// (`voice-{user_id}:{device_id}`, #140).
pub async fn get_device_id(state: &Arc<AppState>) -> Result<Option<String>> {
    Ok(state.device_id.lock().await.clone())
}

/// Rehydrate the current user's profile on boot from `accounts.json`'s
/// `last_active_user`.
pub async fn get_session(state: &Arc<AppState>) -> Result<Option<UserProfile>> {
    // Dev only: `DEV_EMAIL` in `.env.development` auto-logs in on every start.
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

    // The address is a keystore read (#997); empty on a miss, as before.
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

    // One unauthenticated probe (#987 §6.5): this runs before PIN entry, with no
    // credential. "User exists" fails open, so a flaky network doesn't force
    // re-auth (#247); only a confirmed `exists: false` removes the local record.
    // "Has identity" fails to `false`, skipping the enrollment gate this launch
    // rather than trapping the user; an un-enrolled device still can't read history.
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

    // The local DB opens later, at PIN unlock. A failed device registration must
    // not bounce the user to OTP (#247); it re-registers on the next launch.
    if let Err(e) = register_device(state, &profile.id).await {
        crate::diag!("[session] register_device failed for user {} ({e}); proceeding from accounts.json", profile.id);
    }

    // `has_local_account_identity` counts the locked wrapped-only case, so the gate
    // doesn't fire just because the PIN isn't entered yet. Unknown remote state
    // skips the gate this launch (#247).
    let has_remote_identity = probe.as_ref().is_some_and(|p| p.has_identity);
    profile.enrollment_required =
        enrollment_required_for(state, &profile.id, has_remote_identity).await;

    Ok(Some(profile))
}

/// DEV_EMAIL auto-login. The DS owns auth (#419), so this runs the real
/// bootstrap handshake ([`dev_login_ds`]) and nothing else; the direct-Turso
/// fallback was removed (#910).
#[cfg(debug_assertions)]
async fn dev_login_dispatch(state: &Arc<AppState>, email: String) -> Result<UserProfile> {
    // Canonicalize once so every downstream use sees the same address.
    let email = canonical_login_email(&email);

    // No DS, no login (#910): a DS-less client can't do anything (every write is a
    // `POST /v1/...`), so failing with the reason beats an account that 401s on
    // everything.
    if state.config.pollis_delivery_url.is_none() {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "DEV_EMAIL/dev_login needs a Delivery Service: set POLLIS_DELIVERY_URL \
             (see docs/run-it-yourself.md). The DS owns auth and every remote write, \
             so there is nothing a client can do without one."
        )));
    }
    // An error, not a silent fallback that would hide a `DEV_OTP` mismatch.
    dev_login_ds(state, email).await.map_err(|e| {
        crate::error::Error::Other(anyhow::anyhow!(
            "DEV_EMAIL DS enrollment failed: {e}. Ensure the Delivery Service is running \
             with DEV_OTP set to the same value as this client (default 000000)."
        ))
    })
}

/// The deterministic dev OTP. Must match the DS's `DEV_OTP`.
#[cfg(debug_assertions)]
fn dev_otp_code() -> String {
    std::env::var("DEV_OTP")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "000000".to_string())
}

/// DEV_EMAIL login that enrolls the device with the DS. Like [`verify_otp_ds`],
/// but for a returning device it binds the OTP session to the existing
/// `device_id`, so `register-device` creates the DS row a pre-#419 device lacks.
/// Dev only.
#[cfg(debug_assertions)]
async fn dev_login_ds(state: &Arc<AppState>, email: String) -> Result<UserProfile> {
    // Resolve the account from the local index (#987: pre-unlock, no credential).
    // A miss means this machine never signed into the address, so the standard
    // first-device bootstrap is the right path.
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

    // Bind the session to this device's existing id (or a fresh one), so the
    // session-gated `register-device` authorizes this device's row.
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
            // `establish-identity` writes the identity key, under its CAS guard.
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

    // A reset elsewhere leaves a stale local key; recompute the gate honestly.
    if has_identity {
        reconcile_account_identity(state, &user_id, v.account_id_pub.as_deref()).await;
    }

    // Creates or upserts the DS `user_device` row a pre-#419 device lacks.
    // Idempotent. OS and form factor only, never the hostname.
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

    // The cert pivot (`set_pin`/`unlock` → `ensure_device_cert`) publishes via the
    // session, landing `mls_signature_pub` for later device-signed writes.
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

/// The canonical spelling of a login address for `users`.
///
/// `users.email` is `UNIQUE`, so the string is the identity: untrimmed input
/// missed the existing row and created a second account. Trim only, exactly as
/// the DS does (it lowercases only the OTP store key).
#[doc(hidden)]
pub fn canonical_login_email(email: &str) -> String {
    email.trim().to_string()
}

/// Resolve or create this device's stable `device_id` and set
/// `AppState.device_id`.
async fn register_device(state: &Arc<AppState>, user_id: &str) -> Result<String> {
    let device_id = ensure_device_id(state, user_id, &Ulid::new().to_string()).await?;

    // No remote write: every caller is pre-unlock (no signing key), new devices are
    // registered by `/v1/auth/register-device` in `verify_otp_ds`, and the DS bumps
    // `last_seen` on the first verified signed call
    // (`pollis-delivery/src/last_seen.rs`).
    crate::diag!("[auth] device registered: {device_id}");

    // The device cert is published by `set_pin`/`unlock` via `ensure_device_cert`,
    // once the account key is unlocked.

    Ok(device_id)
}

/// Log out, optionally wiping the per-user DB and identity keys.
pub async fn logout(state: &Arc<AppState>, delete_data: bool) -> Result<()> {
    // A corrupt index has already been renamed aside, so a default index is honest.
    let index = crate::accounts::read_accounts_index().unwrap_or_default();
    let user_id = index.last_active_user;

    // A revoked device is always wiped (#1338): keeping its id would sign the next
    // login in as the revoked device. Only for the account whose device was
    // revoked.
    let revoked = user_id.is_some() && state.revoked_user() == user_id;
    let delete_data = delete_data || revoked;

    // Not cleared yet: the device removal below is device-signed.
    let device_id = state.device_id.lock().await.clone();

    // Remove this device from the registry first, while the signer is still live
    // (device-signed `POST /v1/auth/logout`, self-scoped by the DS). Best-effort; a
    // stale row is overwritten on the next login.
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

    // Wipe the media cache, named from the `user_id` read above:
    // `unload_user_db` has cleared the ambient cache user, so the ambient wipe hit
    // the empty `_anon` directory. A corrupt index wipes everything.
    crate::commands::r2::clear_media_cache(match user_id.as_deref() {
        Some(uid) => crate::commands::r2::CacheScope::User(uid),
        None => crate::commands::r2::CacheScope::Everything,
    });

    // Release any attachment the composer had staged but not sent. These are
    // plaintext bytes the user pasted; they live in memory only, and a session
    // ending is exactly when they stop being anyone's business.
    crate::commands::staging::discard_all_staged();

    // Any URL handed out this session 403s from now on.
    *state.media_server_token.lock().await = None;

    if delete_data {
        if let Some(ref uid) = user_id {
            // Remote device removal already ran above, while the signer was live.
            let _ = state.keystore.delete_for_user("db_key", uid).await;
            let _ = state.keystore.delete_for_user("db_key_wrapped", uid).await;
            let _ = state.keystore.delete_for_user("account_id_key", uid).await;
            let _ = state.keystore.delete_for_user("account_id_key_wrapped", uid).await;
            let _ = state.keystore.delete_for_user("pin_meta", uid).await;
            let _ = state.keystore.delete_for_user(DEVICE_ID_KEY, uid).await;
            let _ = state.keystore.delete_for_user(LOGIN_EMAIL_KEY, uid).await;
            let data_dir = crate::db::local::dirs_path();
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

/// Permanently delete the account (#103): wake every group and DM so members
/// remove this user's leaves, wipe all remote data in one DS transaction, then
/// clear the keystore, local DB and caches.
pub async fn delete_account(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<()> {
    // Phase 1: wake every group and DM. Members commit the leaf removals; the local
    // MLS state is wiped in phase 3 regardless. Enumerated (one DS read scoped to
    // the signer) before the remote wipe removes the memberships.
    let mine = crate::commands::ds_reads::conversations(state, &user_id).await?;

    {
        for gid in &mine.group_ids {
            // `publish_to_room_server` reaches members whether or not this client is in the
            // room; an unevicted leaf is the #1081 defect.
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
            // A deleted account is a leave: others must commit the remove (#1081).
            if let Err(e) = crate::commands::livekit::publish_to_room_server(
                state,
                dm_id,
                crate::commands::livekit_signalling::member_left_conversation_payload(dm_id),
            ).await {
                crate::diag!("[account] membership_changed for DM {dm_id} failed (non-fatal): {e}");
            }
        }
    }

    // Phase 2: the whole remote wipe in one DS transaction (#419); a half-deleted
    // account is corrupt. Still fully enrolled here, so it can sign.
    let body = pollis_api::account::DeleteAccountBody {
        // The DS's no-auth fallback actor (`resolve_actor`); when signed it must equal
        // the signed user.
        user_id: Some(user_id.clone()),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Phase 3: local cleanup.

    state.unload_user_db().await;
    {
        let data_dir = crate::db::local::dirs_path();
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
    *state.device_id.lock().await = None;
    *state.unlock.lock().await = None;

    // Drop everything `logout` drops too (#1256): the media cache (outside the data
    // directory), the media-server token and staged attachments.
    crate::commands::r2::clear_media_cache(crate::commands::r2::CacheScope::User(&user_id));
    *state.media_server_token.lock().await = None;
    crate::commands::staging::discard_all_staged();

    let _ = crate::accounts::remove_account(&user_id);

    crate::diag!("[account] deleted account for user {user_id}");
    Ok(())
}

/// Accounts that have signed in on this device, for the "continue as" picker.
/// Login addresses come from the keystore (#997); a failed read yields
/// `email: None` for that account only, rather than failing the whole call.
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

/// Delete all local data on this computer (databases, keystore entries, accounts
/// index, media cache). Remote data is untouched. For "wipe this computer" or a
/// clean uninstall.
pub async fn wipe_local_data(state: &Arc<AppState>) -> Result<()> {
    state.unload_user_db().await;
    *state.device_id.lock().await = None;
    *state.unlock.lock().await = None;

    // A corrupt index is survivable here.
    let index = crate::accounts::read_accounts_index().unwrap_or_default();

    let per_user_keys = [
        DEVICE_ID_KEY,
        "db_key",
        "db_key_wrapped",
        "account_id_key",
        "account_id_key_wrapped",
        "pin_meta",
        LOGIN_EMAIL_KEY,
    ];
    for account in &index.accounts {
        for key in &per_user_keys {
            let _ = state.keystore.delete_for_user(key, &account.user_id).await;
        }
    }

    let data_dir = crate::db::local::dirs_path();
    if data_dir.exists() {
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    // The media cache is not under the data directory on Linux and Windows, so it
    // must be wiped separately.
    crate::commands::r2::clear_media_cache(crate::commands::r2::CacheScope::Everything);

    // Staged attachments are plaintext in memory; release them.
    crate::commands::staging::discard_all_staged();

    crate::diag!("[wipe] local data wiped for {} account(s)", index.accounts.len());
    Ok(())
}

/// All live registered devices for a user, flagging the current one.
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

/// Revoke one of the caller's devices: tombstone the `user_device` row and drop
/// its key packages, then return while leaf removal continues in the background.
/// The current device can't revoke itself; that is `logout(delete_data: true)`.
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

    // A tombstone, not a hard delete (#372), so leaf verification can tell revoked
    // from lagging and historical certs stay verifiable. One DS transaction bound
    // to the signer.
    let body = pollis_api::account::RevokeDeviceBody {
        device_id: device_id.clone(),
        // Same no-auth fallback actor.
        user_id: Some(user_id.clone()),
    };
    // The response carries the device's name at revocation (#987), for the audit
    // entry.
    let device_name = match crate::commands::mls::ds_post_json(state, &body).await? {
        pollis_api::account::DeviceRevoked::Ok { device_name } => device_name,
        pollis_api::account::DeviceRevoked::NotFound => {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "device not found for this user"
            )))
        }
    };

    // Logged immediately after the tombstone, before the reconcile (#947), so the
    // audit entry doesn't depend on N MLS commits. Best-effort: failing the command
    // here would report a revoked device as not revoked.
    let ev = pollis_api::account::SecurityEventBody {
        kind: "device_revoked".to_string(),
        device_id: Some(device_id.clone()),
        // The name at revocation, reduced to its OS label so a legacy hostname (which
        // can embed an IP) never reaches the log.
        metadata: device_name.map(|n| format!("name={}", crate::util::public_device_name(&n))),
        // Same no-auth fallback actor.
        user_id: Some(user_id.clone()),
    };
    if let Err(e) = crate::commands::mls::ds_post_ok(state, &ev).await {
        crate::diag!("[revoke_device] DS security-event failed (non-fatal): {e}");
    }

    // Advisory sign-out nudge to every device of the user; each re-checks its
    // registration and only the revoked one logs out. A device ignoring it is still
    // evicted at the MLS layer. Sent before the leaf removals.
    let payload = crate::commands::livekit_signalling::device_revoked_payload();
    if let Err(e) =
        crate::commands::livekit::publish_to_user_inbox(state, &user_id, payload).await
    {
        crate::diag!("[revoke_device] inbox nudge failed (non-fatal): {e}");
    }

    // Leaf removal runs in the background: the tombstone already makes the DS refuse
    // the device, and one commit per conversation could hold the UI for minutes.
    // Every member's MLS sweep evicts revoked leaves anyway (`leaf_verdict`), so a
    // removal this task misses still happens.
    spawn_revoked_leaf_removal(state, user_id);

    Ok(())
}

/// Conversations a leaf-removal pass reconciles at once; bounds the DS burst.
const REVOKE_RECONCILE_CONCURRENCY: usize = 4;

/// Per-`AppState` pass state: present while a pass runs, `true` if another revoke
/// landed during it. Keyed by state address so two devices in the flows harness
/// don't share it.
static REVOKE_PASSES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<usize, bool>>,
> = std::sync::LazyLock::new(Default::default);

/// Reconcile every group and DM of `user_id` so every revoked leaf is removed.
/// One pass removes every device revoked by then, so revokes coalesce: one made
/// mid-pass just schedules one more pass.
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

/// The recording half of the `device_revoked` answer, for the flows harness's
/// late-response test (#1338).
#[doc(hidden)]
pub use crate::commands::mls::ds_client::note_revoked_response;

/// Whether this process's current device is the one the DS answered
/// `device_revoked` for (#1338), so a late rejection after a sign-out or account
/// switch never ends a healthy session.
pub async fn is_current_device_revoked(state: &Arc<AppState>) -> Result<bool> {
    let device_id = state.device_id.lock().await.clone();
    let index = crate::accounts::read_accounts_index().unwrap_or_default();
    Ok(match (index.last_active_user, device_id) {
        (Some(user_id), Some(device_id)) => state.is_revoked_device(&user_id, &device_id),
        _ => false,
    })
}

/// Whether this device is still an active device of `user_id`; `false` means it
/// was revoked and should log out. Authoritative, so a forged `device_revoked`
/// nudge can't force a logout. `true` while the device id is unknown (early boot).
pub async fn is_current_device_registered(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<bool> {
    let device_id = match state.device_id.lock().await.clone() {
        Some(d) => d,
        None => return Ok(true),
    };
    // `include_revoked: false` is the load-bearing `revoked_at IS NULL` predicate
    // (#685). A revoked device's own signed read is refused as `device_revoked`,
    // which is this answer (#1338).
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

/// Pre-unlock email-to-`user_id` resolution (#997), run against a bare
/// `AppState` (no DB, unlock or device id), exactly as callers run.
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

    /// A pre-#997 `accounts.json` entry.
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

    /// #419: a returning address resolves with nothing open.
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

    /// Case-insensitive, and two accounts on one domain never collide (as masked
    /// addresses would).
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

    /// An account with no keystore entry is skipped, not matched, and doesn't stall
    /// the scan.
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

    /// `AccountInfo` has no field that can hold an address: the invalid state is
    /// unrepresentable.
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

    /// The upgrade path: a pre-#997 address is adopted into the keystore and then
    /// resolves.
    #[tokio::test]
    async fn a_legacy_address_is_adopted_into_the_keystore_and_then_resolves() {
        let state = locked_state();
        let accounts = vec![
            legacy_account("01USER_A", "alice", "alice@example.com"),
            legacy_account("01USER_B", "bob", "bob@example.com"),
        ];

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

    /// Nothing legacy means no rewrite of the file.
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

/// A DS-chosen `user_id` becomes a directory name downstream, so a malicious one
/// must be stopped at the verify-otp response, before anything is persisted.
#[cfg(test)]
mod server_user_id_is_not_a_path {
    use super::*;
    use crate::config::Config;
    use crate::keystore::{InMemoryKeystore, Keystore};
    use pollis_api::DsRequest as _;

    const HOSTILE_ID: &str = "/home/alice";

    /// A DS answering verify-otp with any `user_id`. Only that route exists, so a
    /// sign-in past the id check fails next with a 404.
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

    /// `"/home/alice"` used to be accepted and handed to the media-cache wipe at
    /// set-PIN. Now the sign-in ends here with nothing persisted.
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

    /// Positive control: a well-formed id passes the check (and fails later), so the
    /// test above can't pass because verify-otp is broken for everyone.
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
        // The DS lowercases only the OTP store key, never `users.email`.
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

    /// `(seed, base64 published public key)` for a real account identity.
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

    /// M2: a forged `account_id_pub` must not delete the local key, but must mark
    /// the device for re-enrollment.
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

    /// A match clears the marker: a retracted claim heals the device.
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

    /// An absent or undecodable value changes nothing.
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

    /// Reconciliation records the value later unwraps are checked against (M1).
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

    #[tokio::test]
    async fn no_remote_identity_means_no_enrollment_gate() {
        let state = test_state();
        assert!(!enrollment_required_for(&state, "u1", false).await);
    }
}
