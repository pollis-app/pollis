use serde::{Deserialize, Serialize};

use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;
use ulid::Ulid;

const DEVICE_ID_KEY: &str = "device_id";

/// Per-user keystore slot holding this account's login address (#997).
///
/// It used to sit in plaintext in `accounts.json` — the one field on the device
/// that maps an opaque account id to a person, and the only one outside every
/// protection the product has. The keystore is the right home and costs nothing:
/// every reader of the address already runs at a moment where the keystore is
/// reachable, including [`stable_device_id_for_reenrollment`], which reads
/// [`DEVICE_ID_KEY`] from it at exactly this pre-unlock point.
const LOGIN_EMAIL_KEY: &str = "login_email";

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
pub async fn initialize_identity(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<IdentityInfo> {
    let device_id = state.device_id.lock().await.clone()
        .ok_or_else(|| anyhow::anyhow!("device_id not set — login incomplete"))?;

    match crate::commands::mls::ensure_mls_key_package(state, &user_id, &device_id).await {
        Ok(()) => eprintln!("[identity] MLS key package ensured for {user_id} device {device_id}"),
        Err(e) => eprintln!("[identity] MLS key package error (non-fatal): {e}"),
    }

    // Re-process any pending MLS Welcome messages. This is a no-op when the
    // local MLS state is intact, but recovers group membership after the local
    // DB is wiped (e.g. schema version bump) because load_user_db resets
    // delivered = 0 for this user's welcomes in that case.
    match crate::commands::mls::poll_mls_welcomes_inner(state, &user_id, &device_id).await {
        Ok(()) => {}
        Err(e) => eprintln!("[identity] poll_mls_welcomes (non-fatal): {e}"),
    }

    Ok(IdentityInfo { user_id, public_key: String::new(), is_new: false })
}

/// Check whether an MLS identity exists locally.
pub async fn get_identity() -> Result<Option<IdentityInfo>> {
    Ok(None)
}

/// Request an OTP to be sent to the given email address.
///
/// The OTP is generated, stored, and emailed server-side by the Delivery
/// Service (`POST /v1/auth/request-otp`) — the client never touches
/// `state.otp_store` or a Resend key. See `docs/otp-server-bootstrap-design.md`.
pub async fn request_otp(
    state: &Arc<AppState>,
    email: String,
) -> Result<()> {
    eprintln!(
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
    eprintln!("[auth] request_otp: DS responded HTTP {status}");
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("request-otp failed ({status}): {txt}").into());
    }
    Ok(())
}

/// Verify an OTP code, create or load the user, and complete first-device
/// onboarding.
///
/// The OTP validation, account create/load, account-identity establishment, and
/// device registration all run server-side behind a server-validated
/// OTP-session token ([`verify_otp_ds`]). See
/// `docs/otp-server-bootstrap-design.md`.
pub async fn verify_otp(
    state: &Arc<AppState>,
    email: String,
    code: String,
) -> Result<UserProfile> {
    verify_otp_ds(state, email, code).await
}

/// First-device-signup onboarding routed through the Delivery Service.
///
/// `verify-otp` (DS) validates the code, creates/loads the account, and mints a
/// short-lived OTP-session token. With that token the client then orchestrates
/// the credential-establishing bootstrap writes — none of which can be
/// device-signed because they *establish* the device's signing credential:
///
///   1. `establish-identity` (session-gated) — the version-1 account identity.
///      The client keeps the private `account_id_key` (in `state.unlock`) and
///      sends only the public + Secret-Key-wrapped material.
///   2. `register-device` (session-gated) — the `user_device` row + watermarks.
///   3. the session token is stashed in `state.bootstrap_session` so the
///      pivot write — the first `publish-device-cert`, driven later by `set_pin`
///      → `ensure_device_cert` — can present it.
///
/// A returning device with an established identity (`has_identity`) is a
/// re-login / enrollment, which is OUT OF SCOPE for the DS bootstrap this slice:
/// it registers its device on the direct path and obtains the account key via
/// the existing sibling-approval / Secret-Key flows.
async fn verify_otp_ds(
    state: &Arc<AppState>,
    email: String,
    code: String,
) -> Result<UserProfile> {
    // The device_id the session binds to. Normally a fresh candidate (a
    // brand-new signup has no keystore slot yet, so this becomes the device's
    // stable id once persisted below).
    //
    // EXCEPTION — a returning device that is NOT yet locally enrolled: bind the
    // session to its EXISTING stable id instead. The returning-device branch
    // below only mints an `enrollment_session` when the session's device
    // matches the keystore, and a fresh throwaway candidate never matches after
    // the first login — which left a device that registered once but never
    // finished sibling-approval enrollment UNABLE to ever restart it (the
    // enrollment-request write is session-gated and the session was bound to
    // the wrong device). Binding to the stable id fixes that; `register-device`
    // is an idempotent upsert, so re-registering the existing row is safe.
    // Already-enrolled and brand-new devices keep the fresh candidate (unchanged).
    let candidate_device_id = stable_device_id_for_reenrollment(state, &email)
        .await
        .unwrap_or_else(|| Ulid::new().to_string());

    // 1. verify-otp: DS validates the OTP, creates/loads the account, mints a
    //    session. Unauthenticated (the OTP itself is the proof).
    eprintln!(
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
            // verify-otp never writes the identity key — `establish-identity`
            // does, under its CAS guard. Accepted here and ignored.
            account_id_pub: None,
        },
    )
    .await?;
    let status = resp.status();
    eprintln!("[auth] verify_otp: DS responded HTTP {status}");
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
    // Typed through `pollis-api` (#922) rather than indexed out of a
    // `serde_json::Value`. `v["session_token"]` compiled whatever the DS chose
    // to call that field, so a rename reached users as "response missing
    // session_token" on every sign-in; now it does not build.
    let v =
        crate::commands::mls::decode_response::<pollis_api::otp::VerifyOtpBody>(resp).await?;

    let user_id = v.user_id;
    let username = v.username;
    let has_identity = v.has_identity;
    let session_token = v.session_token;

    // Orphan-detection for a returning device whose server identity no longer
    // matches the local key (a reset elsewhere).
    if has_identity {
        reconcile_account_identity(state, &user_id, v.account_id_pub.as_deref()).await;
    }

    let new_secret_key = if !has_identity {
        eprintln!("[auth] verify_otp: first-device path (has_identity=false) user_id={user_id}");
        // FORCE this device's id to the session-bound candidate — do NOT preserve
        // whatever the keystore holds. On a genuine first signup the slot is empty,
        // so this is identical to `ensure_device_id`. But on a RETRY after a
        // partial-bootstrap failure (a transient establish-identity error, a
        // dropped connection, etc.) the keystore still holds the ABORTED attempt's
        // candidate, while this call's verify-otp minted a session bound to a FRESH
        // candidate. Preserving the stale id makes register-device send a device_id
        // the session doesn't authorize, which the DS rejects as Forbidden — and it
        // stays wedged on every retry until the keychain is cleared by hand.
        // `has_identity == false` guarantees no device was ever registered
        // (register-device runs AFTER establish-identity, which is what sets the
        // identity), so the stale id is worthless and overwriting it is safe. This
        // makes first-device signup idempotent under retry.
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
                    eprintln!("[auth] verify_otp: generate_account_identity_material FAILED: {e}");
                    e
                })?;
        eprintln!("[auth] verify_otp: identity material ready (device_id={device_id}) → establish-identity");

        // 2. establish-identity (session-gated): the DS does the CAS UPDATE +
        //    account_key_log v1 + account_recovery insert.
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
            eprintln!("[auth] verify_otp: establish-identity FAILED: {e}");
            e
        })?;
        eprintln!("[auth] verify_otp: establish-identity OK → register-device");

        // 3. register-device (session-gated): the DS inserts the user_device row
        //    + seeds watermarks.
        let hostname = gethostname::gethostname().to_string_lossy().to_string();
        let device_name = format!("{hostname} ({})", std::env::consts::OS);
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
            eprintln!("[auth] verify_otp: register-device FAILED: {e}");
            e
        })?;
        eprintln!("[auth] verify_otp: register-device OK (first-device bootstrap complete)");

        // Stash the session so the pivot — the first publish-device-cert, run by
        // set_pin → ensure_device_cert — can present it.
        *state.bootstrap_session.lock().await = Some(session_token.clone());

        Some(secret_key)
    } else {
        // Re-login / subsequent device for an EXISTING account. The DS just minted
        // a session (verify-otp). Resolve this device's id and route its
        // `register-device` write through the session-gated DS endpoint — the
        // device has no signing credential yet (its `mls_signature_pub` is NULL),
        // so it cannot device-sign. The account key arrives next via sibling
        // approval (`start_device_enrollment`) or Secret-Key recovery; the FIRST
        // cert publish is then gated by cert-validity ALONE (not this session),
        // since approval can outlast the session TTL. See
        // `docs/otp-server-bootstrap-design.md` §5.
        let device_id = ensure_device_id(state, &user_id, &candidate_device_id).await?;
        if device_id == candidate_device_id {
            // Brand-new device for this account (nothing in the keystore yet): the
            // session minted above is bound to exactly this device_id, so it can
            // authorize the row.
            let hostname = gethostname::gethostname().to_string_lossy().to_string();
            let device_name = format!("{hostname} ({})", std::env::consts::OS);
            crate::commands::mls::ds_post_session_ok(
                state,
                &session_token,
                &pollis_api::bootstrap::RegisterDeviceBody {
                    device_id: device_id.clone(),
                    device_name: Some(device_name),
                },
            )
            .await?;
            // Hold the session for the session-gated enrollment REQUEST write that
            // a sibling-approval enrollment performs next (also pre-credential).
            // NOT stashed in `bootstrap_session`: the cert publish must stay on the
            // cert-validity-alone path.
            *state.enrollment_session.lock().await = Some(session_token.clone());
        } else {
            // A device already known to this account (its registered device_id
            // predates this session, which is bound to a fresh candidate). The
            // session can't authorize it and the local DB is still closed (no
            // signing key), so `register_device` no-ops its remote write under a DS
            // (bucket-C C2) — the row already exists with a cert from a prior
            // login; the `last_seen` touch it skips is non-critical. It still
            // resolves + sets `state.device_id`.
            register_device(state, &user_id).await?;
        }
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

    crate::accounts::upsert_account(&profile.id, &profile.username, None)?;
    store_login_email(state, &profile.id, &profile.email).await;

    Ok(profile)
}

/// Drop this device's local account key when the server's account identity is a
/// DIFFERENT one — the signature of a reset performed on another device.
///
/// `account_id_pub` is the value the just-completed verify-otp reported for this
/// account (#987); it used to be a second, direct `SELECT` from the client.
///
/// Fails SAFE in both directions: an absent or unparseable value leaves the
/// local key alone (a wipe is destructive and must never be triggered by a
/// missing field), and only a value that decodes AND fails to match wipes.
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
        eprintln!("[auth] verify-otp returned an undecodable account_id_pub; leaving local key");
        return;
    };
    let matches = crate::commands::account_identity::has_matching_local_account_identity(
        state, user_id, &pub_bytes,
    )
    .await
    .unwrap_or(false);
    if matches {
        return;
    }
    if let Err(e) =
        crate::commands::account_identity::wipe_local_account_identity(state, user_id).await
    {
        eprintln!("[auth] wipe_local_account_identity (non-fatal): {e}");
    }
}

/// Resolve this device's stable `device_id` for `user_id`, persisting `candidate`
/// in the keystore if none exists yet, and store it in `AppState.device_id`.
/// Extracted so the DS bootstrap path can determine the device_id (to bind the
/// OTP session to) without doing the remote `user_device` write that
/// [`register_device`] does directly.
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
///
/// `None` on ANY failure, deliberately. The whitepaper §4 records that transient
/// keychain / secret-service errors used to bounce returning users back to OTP,
/// so a hiccup here must degrade — to "type your email", which is the screen a
/// brand-new device sees anyway — and never propagate as an error out of a call
/// whose other results are perfectly good.
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
///
/// Best-effort for the same reason [`login_email_for_user`] is: a keystore
/// hiccup must not fail a login that has otherwise fully succeeded. A miss costs
/// the login screen's one-click chip and the returning-device `device_id` reuse,
/// and both re-establish on the next successful login.
///
/// Returns whether the address actually landed, which only the upgrade path
/// below cares about — it must not erase the plaintext copy on disk until the
/// keystore has taken one.
async fn store_login_email(state: &Arc<AppState>, user_id: &str, email: &str) -> bool {
    match state
        .keystore
        .store_for_user(LOGIN_EMAIL_KEY, user_id, email.as_bytes())
        .await
    {
        Ok(()) => true,
        Err(e) => {
            eprintln!("[auth] could not persist the login address for {user_id}: {e}");
            false
        }
    }
}

/// Read the accounts index, first moving any pre-#997 plaintext login address it
/// still carries into the per-user keystore and erasing it from the file.
///
/// This is the upgrade path, and it is load-bearing rather than cosmetic. Every
/// install that predates #997 has the address in `accounts.json` and nowhere
/// else; without this it would lose the pre-unlock email → `user_id` resolution
/// — #419's returning-device path — and the login screen's "continue as" chip
/// until the user signed in by OTP again. It runs at the three places that need
/// the address, all of which are already async with `state` in hand.
///
/// Ordering is deliberate: keystore write first, disk erase second. An address
/// already in the keystore wins over the file's copy, because every writer since
/// the upgrade has kept the keystore current. If ANY account's keystore write
/// fails, nothing is erased and the next launch simply tries again — a failed
/// migration must never be a lost address.
async fn read_accounts_index_migrated(
    state: &Arc<AppState>,
) -> Result<crate::accounts::AccountsIndex> {
    let index = crate::accounts::read_accounts_index()?;

    if adopt_legacy_login_emails(state, &index.accounts).await {
        if let Err(e) = crate::accounts::forget_legacy_emails() {
            eprintln!("[auth] could not erase legacy login addresses from accounts.json: {e}");
        }
    }

    Ok(index)
}

/// Move every pre-#997 address these accounts still carry into the keystore.
///
/// Returns whether the file is now safe to rewrite without them: true only when
/// at least one legacy address was seen AND every one of them is now in the
/// keystore. Split from [`read_accounts_index_migrated`] at the index read, for
/// the same reason [`user_id_for_email_among`] is — the keystore is hermetic per
/// `AppState`, the on-disk index is process-global.
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
        // A keystore entry wins over the file's copy: every writer since the
        // upgrade has kept the keystore current, and the file's is frozen at
        // whatever the last pre-upgrade login wrote.
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
///
/// Split off from [`local_user_id_for_email`] at the index read so the matching
/// rule itself is testable: the index lives at the process-global data dir,
/// which unit tests in this crate cannot claim without fighting each other over
/// it, while the keystore is per-`AppState` and therefore hermetic.
///
/// N is the number of accounts that have signed in on this device — three or so
/// at the outside — so the linear scan of keystore reads is not worth indexing.
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
///
/// The only such lookup available pre-unlock: the local DB is closed (so there
/// is no device signer) and no OTP has run (so there is no session), which
/// leaves no credential for a DS read — and #987 removed the direct
/// `SELECT id FROM users WHERE email = ?` that used to serve these call sites.
/// `None` means "this machine has not signed into that address before", which is
/// exactly the condition under which both callers want the fresh-device path.
///
/// The candidate `user_id`s come from `accounts.json`; the addresses they are
/// matched against come from the keystore (#997), which is readable at this
/// moment for exactly the reason [`stable_device_id_for_reenrollment`] — the
/// caller below — can already read [`DEVICE_ID_KEY`] out of it here.
async fn local_user_id_for_email(state: &Arc<AppState>, email: &str) -> Option<String> {
    let index = read_accounts_index_migrated(state).await.ok()?;
    user_id_for_email_among(state, &index.accounts, email).await
}

/// For a RETURNING, not-yet-enrolled device, return its existing stable
/// `device_id` so `verify_otp_ds` can bind the OTP session to it (rather than a
/// throwaway candidate) — the prerequisite for minting an `enrollment_session`
/// on a device that already registered once. Returns `None` — so the caller
/// falls back to a fresh id — when there's no local account for this email (a
/// brand-new device), when the device is already enrolled (preserve the current
/// fresh-candidate path), or when no keystore device_id exists yet.
///
/// user_id is resolved from the LOCAL accounts index (a returning device has an
/// entry from a prior login), so this adds no network round-trip. The
/// enrollment check reads the keystore, not the still-locked DB, so it is valid
/// here pre-unlock (same call site semantics as the `enrollment_required` calc).
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
///
/// The request is DEVICE-SIGNED and routed to
/// `POST /v1/auth/request-email-change-otp` — the DS records `(authed user →
/// new_email)`, generates/stores/emails the OTP keyed on the new email, and the
/// client never touches `state.otp_store`, the Resend key, or `users`. The DS
/// always 200s (anti-enumeration), so the uniqueness check happens at verify
/// time. See `docs/otp-server-bootstrap-design.md`.
///
/// #987 deleted the no-DS fallback that used to sit below this. It pre-checked
/// uniqueness with a direct `SELECT id FROM users WHERE email = ?` and then ran
/// the client-side OTP path — a path unreachable in any real deployment since
/// #419 moved OTP server-side, and one that cannot exist at all now the client
/// holds no database credential. Nothing is lost by its removal: the pre-check
/// was a TOCTOU against a mailbox another account could claim in the interval,
/// so verify time is where uniqueness has to be decided regardless.
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

/// Verify the OTP sent to a new email address and atomically swap
/// `users.email` for the calling user. The OTP proves the caller controls
/// the new mailbox; the device's signing identity proves they're the one
/// asking.
///
/// The OTP validation, requester binding, and `UPDATE users` all run server-side
/// behind the device-signature gate (`POST /v1/auth/verify-email-change`) — the
/// authed user is bound from the signature, never the body. The client then
/// mirrors the new address into the local `accounts.json` index (a device-local
/// file the DS can't touch).
pub async fn verify_email_change(
    state: &Arc<AppState>,
    user_id: String,
    new_email: String,
    code: String,
) -> Result<()> {
    let trimmed = new_email.trim().to_string();

    let resp = crate::commands::mls::ds_post(
        state,
        &pollis_api::email_change::VerifyEmailChangeBody {
            new_email: trimmed.clone(),
            code,
        },
    )
    .await?;
    let username: Option<String> = match resp.status().as_u16() {
        200 => {
            // The username rides on the swap (#987): the accounts-index mirror
            // below needs it, and reading it back afterwards was a round trip
            // for a row the DS had just written — one that could observe a
            // rename in between and stamp it as part of this change.
            let pollis_api::email_change::EmailChanged::Ok { username } =
                crate::commands::mls::decode_response::<
                    pollis_api::email_change::VerifyEmailChangeBody,
                >(resp)
                .await?;
            username
        }
        401 => return Err(anyhow::anyhow!("Invalid code. Please check and try again.").into()),
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

    // Mirror the new email into the keystore so the login screen's "continue as"
    // picker offers the new address, and so the pre-unlock
    // `local_user_id_for_email` resolution follows the account. Runs after the
    // swap succeeds. Touch the accounts index too — it carries the `last_seen`
    // and username this flow can also have changed.
    if let Some(username) = username {
        if let Err(e) = crate::accounts::upsert_account(&user_id, &username, None) {
            eprintln!("[auth] email-change accounts.json mirror failed: {e}");
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
/// Return this device's stable `device_id` (the one registered in
/// `user_device` and used to scope per-device identities), or `None` if the
/// device hasn't been registered yet this process. The frontend uses it to
/// build its own per-device voice identity (`voice-{user_id}:{device_id}`) so
/// it can tell which voice participant is itself (#140).
pub async fn get_device_id(state: &Arc<AppState>) -> Result<Option<String>> {
    Ok(state.device_id.lock().await.clone())
}

pub async fn get_session(state: &Arc<AppState>) -> Result<Option<UserProfile>> {
    // In development, DEV_EMAIL bypasses the normal session check and logs in directly.
    // Set DEV_EMAIL=user@example.com in .env.development to auto-login on every startup.
    #[cfg(debug_assertions)]
    if let Ok(dev_email) = std::env::var("DEV_EMAIL") {
        let dev_email = dev_email.trim().to_string();
        if !dev_email.is_empty() {
            eprintln!("[auth] DEV_EMAIL active — auto-logging in as {dev_email}");
            let profile = dev_login_dispatch(state, dev_email).await?;
            return Ok(Some(profile));
        }
    }

    // Identify the last active user from the local accounts index.
    let index = read_accounts_index_migrated(state).await?;
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

    // The address is a keystore read now (#997), not a field of the index. Empty
    // on a keystore miss, exactly as an absent `email` field used to be — the
    // downstream `UserProfile.email` contract is unchanged.
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

    // Verify the user still exists, and recompute the enrollment gate — ONE
    // probe for both (#987 §6.5).
    //
    // This runs before PIN entry, so there is no credential of any kind: the
    // local DB is closed (no signer), `device_id` is unset, and no OTP has run.
    // Hence the unauthenticated probe. Both answers keep the biases they had:
    //
    //   * "user exists" fails OPEN — a network failure is "assume valid", so a
    //     flaky connection at startup does not force re-authentication (#247).
    //     Only a CONFIRMED `exists: false` removes the local record, which is
    //     what a genuine account deletion elsewhere looks like.
    //   * "has identity" fails to `false` — an unknown remote state skips the
    //     enrollment gate this launch rather than trapping a logged-in user
    //     behind it. An un-enrolled device still cannot read history (the keys
    //     are not local); it is simply not *forced* through the gate offline.
    let probe = match crate::commands::ds_reads::account_probe(state, &profile.id).await {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("[session] account probe failed ({e}); proceeding from accounts.json");
            None
        }
    };
    if probe.as_ref().is_some_and(|p| !p.exists) {
        let _ = crate::accounts::remove_account(&profile.id);
        return Ok(None);
    }

    // The local DB is NOT opened here. The frontend's next call is
    // get_unlock_state; if pin_set && !is_unlocked it routes to the
    // PIN entry screen, where unlock opens the DB under the
    // unwrapped db_key. The orphan recompute (was here before #194)
    // moves with it — has_matching_local_account_identity can't
    // verify against a still-locked key anyway.
    // Device registration is a fire-and-forget Turso/keystore refresh
    // whose return value is unused here. A transient failure at launch
    // must NOT bounce a logged-in user back to OTP (issue #247) — the
    // row re-registers on the next reachable launch. This mirrors the
    // network-tolerant user-existence check above.
    if let Err(e) = register_device(state, &profile.id).await {
        eprintln!("[session] register_device failed for user {} ({e}); proceeding from accounts.json", profile.id);
    }

    // Conservative: if the wrapped slot is absent AND no plaintext
    // copy exists, this device is orphaned/unenrolled. has_local_
    // account_identity returns true for the wrapped-only (locked)
    // case, so we don't false-positive an enrollment gate just
    // because the user hasn't entered their PIN yet.
    // A transient Turso failure here must NOT bounce a logged-in user
    // back to OTP (issue #247). Unknown remote state is treated as
    // `None`, which yields `enrollment_required = false` below: the user
    // proceeds to PIN unlock and the enrollment gate recomputes on the
    // next reachable launch. An un-enrolled device still cannot read
    // history (keys are not local) — it is simply not *forced* through
    // the gate while offline. Consistent with the network-tolerant
    // user-existence check above; only a confirmed `None` row (user
    // genuinely has no remote pubkey) and a present row are decisive.
    let has_remote_identity = probe.as_ref().is_some_and(|p| p.has_identity);
    let has_local_identity = crate::commands::account_identity::has_local_account_identity(
        state,
        &profile.id,
    )
    .await
    .unwrap_or(false);
    profile.enrollment_required = has_remote_identity && !has_local_identity;

    Ok(Some(profile))
}

/// DEV_EMAIL auto-login entry point.
///
/// The Delivery Service OWNS auth/bootstrap (#419): it authorizes every write by
/// the device's registered `mls_signature_pub`, so a device that never ran the
/// bootstrap handshake (verify-otp → register-device → publish-device-cert) has
/// no DS-recognized credential and every DS write 401s. This drives that real
/// handshake ([`dev_login_ds`]) and nothing else.
///
/// It used to have a second path: a `dev_login_inner` that INSERTed the `users`
/// row straight into Turso, taken when no DS was configured and as a fall-back
/// when DS enrollment failed. #910 removed it. It was the last client-side
/// remote write in the codebase — the one live counter-example to a rule
/// (`CLAUDE.md`: "Never add a client-side remote INSERT/UPDATE/DELETE — extend a
/// DS endpoint") that everything else follows — and as a fall-back it actively
/// hid the misconfiguration it warned about, producing a device that looked
/// signed in and 401'd on every subsequent write.
#[cfg(debug_assertions)]
async fn dev_login_dispatch(state: &Arc<AppState>, email: String) -> Result<UserProfile> {
    // Canonicalize once, up front, so every downstream use — the `users`
    // lookup, the DS verify-otp body, `accounts.json`, the returned profile —
    // sees the same address.
    let email = canonical_login_email(&email);

    // No DS, no login (#910). The Turso-direct shortcut this replaces was the
    // one client-side remote INSERT left in the codebase, and CLAUDE.md bans
    // those outright — the DS is the sole writer.
    //
    // Nothing is lost by refusing. A DS-less client cannot send a message,
    // create a group, or register a device either: every one of those is a
    // `POST /v1/...`. So "logged in without a DS" was never a working state, it
    // was a state that looked like one until the first write failed. Failing
    // here, with the reason, is strictly more useful than a local account that
    // 401s on everything.
    if state.config.pollis_delivery_url.is_none() {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "DEV_EMAIL/dev_login needs a Delivery Service: set POLLIS_DELIVERY_URL \
             (see docs/run-it-yourself.md). The DS owns auth and every remote write, \
             so there is nothing a client can do without one."
        )));
    }
    // A DS enrollment failure is now an ERROR, not a silent fall-back. The old
    // fall-back masked exactly the misconfiguration it printed about — a DS
    // running with a different DEV_OTP produced a device that looked signed in
    // and 401'd on every write.
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

/// DEV_EMAIL auto-login that ENROLLS the device with the Delivery Service, so a
/// pre-#419 dev device (or a fresh dev machine) obtains a DS-recognized credential
/// instead of 401ing on every DS write.
///
/// Mirrors the production [`verify_otp_ds`] handshake, with one dev-only twist that
/// makes it work for a RETURNING device (the case production leaves to
/// sibling-approval / Secret-Key enrollment): it binds the OTP session to this
/// device's EXISTING `device_id` (resolved from the keystore before verify-otp),
/// so the subsequent session-gated `register-device` can create the DS
/// `user_device` row for a device that already exists locally. Production's
/// returning-device branch can't do this — its session is bound to a throwaway
/// candidate id — which is why a pre-#419 device never gets a DS row and its
/// cert-validity `publish-device-cert` 401s.
///
/// Strictly dev-only (`#[cfg(debug_assertions)]`); it does not touch the
/// production bootstrap invariants.
#[cfg(debug_assertions)]
async fn dev_login_ds(state: &Arc<AppState>, email: String) -> Result<UserProfile> {
    // Pre-resolve the account so we can reuse this device's stable device_id and
    // bind the OTP session to it. Resolved from the LOCAL accounts index (#987):
    // this runs pre-unlock, where no credential of any kind exists, and the
    // index is written by every prior successful login on this machine.
    //
    // A miss means this machine has never signed into that address — so the
    // keystore holds no device_id for it either, the "returning device" twist
    // below would degenerate into a fresh candidate anyway, and the standard
    // first-device DS bootstrap (verify_otp_ds via verify_otp) is the correct
    // path. That covers both a brand-new dev account and a clean dev machine.
    let user_id = match local_user_id_for_email(state, &email).await {
        Some(uid) => uid,
        None => {
            eprintln!(
                "[auth] DEV_EMAIL: no local account for {email}; first-device DS bootstrap"
            );
            request_otp(state, email.clone()).await?;
            return verify_otp(state, email, dev_otp_code()).await;
        }
    };

    // Bind the session to this device's existing id (or a fresh candidate on a
    // clean machine). Sending it in the verify-otp body is what lets the
    // session-gated register-device below authorize THIS device's row.
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
            // verify-otp never writes the identity key — `establish-identity`
            // does, under its CAS guard. Accepted here and ignored.
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

    // Persist + publish the bound device_id into state.
    let device_id = ensure_device_id(state, &user_id, &bound_device_id).await?;

    // Orphan-detection: a reset elsewhere leaves a stale local key, so the
    // enrollment gate recomputes honestly.
    if has_identity {
        reconcile_account_identity(state, &user_id, v.account_id_pub.as_deref()).await;
    }

    // register-device (session-gated): creates/upserts the DS `user_device` row —
    // the row a pre-#419 device is missing. Idempotent.
    let hostname = gethostname::gethostname().to_string_lossy().to_string();
    let device_name = format!("{hostname} ({})", std::env::consts::OS);
    crate::commands::mls::ds_post_session_ok(
        state,
        &session_token,
        &pollis_api::bootstrap::RegisterDeviceBody {
            device_id: device_id.clone(),
            device_name: Some(device_name),
        },
    )
    .await?;

    // Stash the session so the cert pivot (set_pin / unlock → ensure_device_cert)
    // publishes via the session path — which both authorizes the write and, with
    // the row now present, lands `mls_signature_pub` so later device-signed DS
    // writes (GroupInfo, external-join) authenticate.
    *state.bootstrap_session.lock().await = Some(session_token);

    let enrollment_required = has_identity
        && !crate::commands::account_identity::has_local_account_identity(state, &user_id)
            .await
            .unwrap_or(false);

    crate::accounts::upsert_account(&user_id, &username, None)?;
    store_login_email(state, &user_id, &email).await;

    eprintln!("[auth] DEV_EMAIL: DS-enrolled device {device_id} for {user_id}");
    Ok(UserProfile {
        id: user_id,
        email,
        username,
        new_secret_key: None,
        enrollment_required,
    })
}

/// The canonical spelling of a login address for the `users` table.
///
/// `users.email` is `NOT NULL UNIQUE`, so the *string* is the account's
/// identity. The Delivery Service trims before it touches that table
/// (`pollis_delivery::otp::verify_otp`); the client path did not, so
/// `" a@x.com "` missed the row `"a@x.com"` owns and then INSERTed a **second**
/// account for the same person — an invalid state UNIQUE cannot catch, because
/// the two strings genuinely differ.
///
/// Trim only. The DS deliberately does *not* lowercase for `users` (it
/// lowercases only the OTP store key — see `otp::normalize_email`), and the
/// point of this function is to match the DS exactly, not to be independently
/// sensible.
#[doc(hidden)]
pub fn canonical_login_email(email: &str) -> String {
    email.trim().to_string()
}

/// Register this device for the given user. Generates a stable device_id on first
/// call (persisted in the OS keystore), inserts/updates the remote `user_device`
/// table, and stores the id in `AppState.device_id`.
async fn register_device(state: &Arc<AppState>, user_id: &str) -> Result<String> {
    // Resolve-or-create the stable device_id (also sets `state.device_id`).
    let device_id = ensure_device_id(state, user_id, &Ulid::new().to_string()).await?;

    // The remote `user_device` + watermark writes are owned by the Delivery
    // Service now. A brand-new device's row is created by the session-gated
    // `/v1/auth/register-device` in `verify_otp_ds`; every caller that reaches
    // *this* fn does so at a PRE-UNLOCK point (the known-device re-login branch of
    // `verify_otp_ds`, the session-resume `get_session`) where the local DB is
    // still closed and no device signing key is loadable. The old direct
    // `last_seen` touch + watermark backfill on an ALREADY-existing row could
    // neither be device-signed nor performed under a read-only Turso token, so it
    // is dropped. `ensure_device_id` above already set `state.device_id`, so the
    // rest of the flow (and later DS calls) still have it.
    eprintln!("[auth] device registered: {device_id}");

    // Device-cert publish moved out of register_device. It needs the
    // unwrapped account_id_key, which post-#194 lives in AppState.unlock
    // and isn't populated until set_pin / unlock runs. set_pin and
    // unlock both invoke ensure_device_cert as part of the same
    // roundtrip so the cert lands at the natural moment.

    Ok(device_id)
}

/// Clear the persisted session (logout). Optionally wipe the per-user DB and identity keys.
pub async fn logout(state: &Arc<AppState>, delete_data: bool) -> Result<()> {
    // If the index is corrupt we still want the user to be able to log out.
    // `accounts.json` has already been renamed to `.bad-<ts>.json` by the
    // read path, so a default (empty) index is the honest state of the world.
    let index = crate::accounts::read_accounts_index().unwrap_or_default();
    let user_id = index.last_active_user;

    // Snapshot the device_id WITHOUT clearing it yet — a DS-routed device
    // removal (below) is device-signed and needs `state.device_id` still set.
    let device_id = state.device_id.lock().await.clone();

    // Remove this device from the remote registry FIRST, while the local DB, the
    // device signing key, and the in-memory session are all still live.
    //
    // Bucket-C C4: this is a DEVICE-SIGNED `POST /v1/auth/logout` — a self-scoped
    // DELETE of THIS device's `user_device` row (the DS binds it to the
    // authenticated signer, so a caller can only log out their own account's
    // device). It MUST run before `unload_user_db` / the `unlock` clear below:
    // once those run the signer is gone and a device-signed request is impossible.
    // Best-effort: a stale row left behind on a network failure is harmless
    // (re-registered/overwritten on next login).
    if delete_data {
        if let (Some(uid), Some(did)) = (user_id.as_ref(), device_id.as_ref()) {
            let body = pollis_api::account::LogoutDeviceBody {
                device_id: did.clone(),
                user_id: Some(uid.clone()),
            };
            if let Err(e) = crate::commands::mls::ds_post_ok(state, &body).await {
                eprintln!("[auth] logout device removal via DS failed (non-fatal): {e}");
            }
        }
    }

    // Now tear down local state.
    *state.device_id.lock().await = None;
    state.unload_user_db().await;
    *state.unlock.lock().await = None;

    // Wipe the on-disk media cache. Files are AES-GCM-encrypted under
    // the active session's db_key, so they would already be opaque to
    // any future user — but logout is the natural lifecycle boundary
    // and an empty cache costs nothing.
    //
    // Named explicitly, from the `user_id` this function read out of the
    // accounts index at the top: `unload_user_db` above has already cleared the
    // ambient cache user, and a wipe that read it would resolve the empty
    // `_anon` directory and leave this user's media exactly where it was. An
    // index too corrupt to name anyone means nobody can say which directory is
    // this session's, so all of them go.
    crate::commands::r2::clear_media_cache(match user_id.as_deref() {
        Some(uid) => crate::commands::r2::CacheScope::User(uid),
        None => crate::commands::r2::CacheScope::Everything,
    });

    // Release any attachment the composer had staged but not sent. These are
    // plaintext bytes the user pasted; they live in memory only, and a session
    // ending is exactly when they stop being anyone's business.
    crate::commands::staging::discard_all_staged();

    // Clear the media-server token. Any URL handed to the webview
    // during this session 403s from here on.
    *state.media_server_token.lock().await = None;

    if delete_data {
        if let Some(ref uid) = user_id {
            // Remote device removal already ran above (while the signer was live).
            let _ = state.keystore.delete_for_user("db_key", uid).await;
            let _ = state.keystore.delete_for_user("db_key_wrapped", uid).await;
            let _ = state.keystore.delete_for_user("account_id_key", uid).await;
            let _ = state.keystore.delete_for_user("account_id_key_wrapped", uid).await;
            let _ = state.keystore.delete_for_user("pin_meta", uid).await;
            let _ = state.keystore.delete_for_user(DEVICE_ID_KEY, uid).await;
            let _ = state.keystore.delete_for_user(LOGIN_EMAIL_KEY, uid).await;
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
pub async fn delete_account(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<()> {
    // ── Phase 1: Signal membership change ────────────────────────────
    // Broadcast membership_changed to all groups and DM channels the user
    // belongs to. Remaining online members will reconcile and remove the
    // deleting user's stale MLS leaves asynchronously. The user's local DB
    // (including MLS state) is wiped in Phase 3, so they can't decrypt
    // regardless.
    //
    // The enumeration is one DS read (#987) — the DS scopes it to the signer,
    // so this can only ever list the caller's OWN conversations. It runs BEFORE
    // the wipe below, which is the last moment the memberships still exist.
    let mine = crate::commands::ds_reads::conversations(state, &user_id).await?;

    {
        for gid in &mine.group_ids {
            if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
                &state.livekit, gid,
            ).await {
                eprintln!("[account] membership_changed for group {gid} failed (non-fatal): {e}");
            }
        }
    }

    {
        for dm_id in &mine.dm_ids {
            if let Err(e) = crate::commands::livekit::publish_to_room_server(
                state,
                dm_id,
                serde_json::json!({"type": "membership_changed", "conversation_id": dm_id}),
            ).await {
                eprintln!("[account] membership_changed for DM {dm_id} failed (non-fatal): {e}");
            }
        }
    }

    // ── Phase 2: remote data cleanup ───────────────────────────────────
    //
    // Route the entire remote wipe through the DS (#419 domains E+G) as ONE
    // transaction when configured: the group-ownership handoff, the sent-envelope
    // / key-package / membership deletes, and the final `users` delete all commit
    // together or not at all — a half-deleted account is a corrupt state. The
    // device is still fully enrolled here (the local DB is unloaded only in Phase
    // 3 below), so it can sign the request.
    let body = pollis_api::account::DeleteAccountBody {
        // The DS's no-auth fallback for the acting user
        // (`pollis_delivery::writes::resolve_actor`): auth on → the signed
        // user and this must EQUAL it; auth off → this IS the actor, and a
        // body without it is refused outright. Sending it never widens what
        // the caller may do.
        user_id: Some(user_id.clone()),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

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
    let _ = state.keystore.delete_for_user(LOGIN_EMAIL_KEY, &user_id).await;
    *state.device_id.lock().await = None;
    *state.unlock.lock().await = None;

    // Remove from local accounts index
    let _ = crate::accounts::remove_account(&user_id);

    eprintln!("[account] deleted account for user {user_id}");
    Ok(())
}

/// Return the list of accounts that have previously signed in on this device.
/// Used by the login screen to show a "continue as" picker.
///
/// The index supplies the identity and display fields; the login address is
/// rehydrated per account from the keystore (#997), which is why this is async
/// and takes `state`. The JSON it produces is identical to what the raw index
/// serialized to before that move, so the renderer is untouched.
///
/// A per-account keystore read that fails yields `email: None` for that account
/// and nothing more — see [`login_email_for_user`]. The picker then renders that
/// chip disabled and the user types their address, which is strictly better than
/// failing the whole call and hiding every OTHER account's chip too.
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
///
/// Intended for the login screen "wipe this computer" action or for
/// preparing a clean uninstall across platforms.
pub async fn wipe_local_data(state: &Arc<AppState>) -> Result<()> {
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
        LOGIN_EMAIL_KEY,
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

    // 5. And the media cache, which is NOT under that directory. The shell
    //    installs the cache root from Tauri's `app_data_dir().join("media-cache")`
    //    — `~/.local/share/pollis/…` on Linux and `%APPDATA%\pollis\…` on
    //    Windows against a `dirs_path()` that resolves elsewhere (only macOS
    //    happens to make the two coincide). Removing the data directory alone
    //    therefore left every cached image, video and voice clip on the disk of a
    //    computer whose owner asked for all of it to be gone.
    crate::commands::r2::clear_media_cache(crate::commands::r2::CacheScope::Everything);

    // Release any attachment the composer had staged but not sent. These are
    // plaintext bytes the user pasted; they live in memory only, and a session
    // ending is exactly when they stop being anyone's business.
    crate::commands::staging::discard_all_staged();

    eprintln!("[wipe] local data wiped for {} account(s)", index.accounts.len());
    Ok(())
}

/// List all registered devices for a user. Returns each device's ID,
/// name, timestamps, and whether it is the current device.
pub async fn list_user_devices(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<Vec<serde_json::Value>> {
    let current_device_id = state.device_id.lock().await.clone();
    // Live devices only (`include_revoked: false`), oldest first — the DS
    // preserves both the `revoked_at IS NULL` filter and the `created_at`
    // ordering this list has always had.
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

/// Revoke one of the calling user's devices. Deletes the `user_device` row
/// and any unclaimed key packages, then drives an MLS reconcile across
/// every group + DM the user is in so the revoked device's leaf is
/// removed from each tree. The current device cannot revoke itself —
/// `logout(delete_data: true)` is the path for that.
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

    // Drop the revoked device's unclaimed key packages (one-time-use, useless
    // now) and tombstone its row (issue #372 — a tombstone, not a hard-delete,
    // so other members' `verify_added_devices` can tell "revoked, drop rejoin"
    // from "lagging, keep commits" and the row stays available for historical
    // cert verification). Routed through the DS (#419 domains E+G) as one
    // transaction when configured — the CURRENT device drives this and is fully
    // enrolled, so it can sign; the DS binds both writes to the signer
    // (`WHERE user_id = actor`).
    let body = pollis_api::account::RevokeDeviceBody {
        device_id: device_id.clone(),
        // The DS's no-auth fallback for the acting user
        // (`pollis_delivery::writes::resolve_actor`): auth on → the signed
        // user and this must EQUAL it; auth off → this IS the actor, and a
        // body without it is refused outright. Sending it never widens what
        // the caller may do.
        user_id: Some(user_id.clone()),
    };
    // The response carries the name the device had at the moment it was revoked
    // (#987) — the audit entry below is the only place it can still be read once
    // the row is tombstoned, and it used to be a separate client SELECT that ran
    // BEFORE the write, so a device revoked concurrently was named by a read
    // that had already gone stale.
    let device_name = match crate::commands::mls::ds_post_json(state, &body).await? {
        pollis_api::account::DeviceRevoked::Ok { device_name } => device_name,
        pollis_api::account::DeviceRevoked::NotFound => {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "device not found for this user"
            )))
        }
    };

    // Append the revocation to the account's own security log (#947).
    //
    // Written HERE — immediately after the tombstone lands, before the
    // reconcile loop below, which walks every conversation and only logs its
    // failures. "I revoked the stolen laptop, did it take effect?" is the
    // first question this flow produces and the Security page is where it is
    // asked; an audit trail that is contingent on N MLS commits succeeding
    // would be missing exactly when the user most needs it.
    //
    // Best-effort, like the enrollment events (`device_enrolled` /
    // `device_rejected`) it sits beside: the revocation itself has already
    // committed and is authoritative, so failing the command on a flaky audit
    // write would report "revocation failed" for a device that is, in fact,
    // revoked — which is the more dangerous of the two lies.
    let ev = pollis_api::account::SecurityEventBody {
        kind: "device_revoked".to_string(),
        device_id: Some(device_id.clone()),
        // The device's name at the moment it was revoked. `user_device` keeps
        // the tombstoned row, but a log that says only "device 01HQ7Z…" is not
        // the answer to "was it the laptop or the phone?".
        metadata: device_name.map(|n| format!("name={n}")),
        // The DS's no-auth fallback for the acting user
        // (`pollis_delivery::writes::resolve_actor`): auth on → the signed
        // user and this must EQUAL it; auth off → this IS the actor, and a
        // body without it is refused outright. Sending it never widens what
        // the caller may do.
        user_id: Some(user_id.clone()),
    };
    if let Err(e) = crate::commands::mls::ds_post_ok(state, &ev).await {
        eprintln!("[revoke_device] DS security-event failed (non-fatal): {e}");
    }

    // Collect every conversation the user belongs to (groups + DMs) so
    // we can reconcile each one as the calling device. One DS read (#987),
    // scoped by the DS to the signer's own memberships.
    let mine = crate::commands::ds_reads::conversations(state, &user_id).await?;
    let conversation_ids: Vec<String> = mine
        .group_ids
        .into_iter()
        .chain(mine.dm_ids)
        .collect();

    for cid in &conversation_ids {
        if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
            state,
            cid,
            &user_id,
        )
        .await
        {
            eprintln!("[revoke_device] reconcile {cid} failed: {e}");
        }
    }

    // Cooperative "you've been signed out" nudge. The inbox is per-user, so
    // this reaches every device; each one re-checks its own registration via
    // `is_current_device_registered` and only the revoked device logs out.
    // Advisory UX only — a hostile/offline device that ignores this is still
    // evicted at the MLS layer (honest members reject its self-add). Non-fatal.
    let payload = crate::commands::livekit_signalling::device_revoked_payload();
    if let Err(e) =
        crate::commands::livekit::publish_to_user_inbox(state, &user_id, payload).await
    {
        eprintln!("[revoke_device] inbox nudge failed (non-fatal): {e}");
    }

    Ok(())
}

/// Whether the current device (`state.device_id`) is still an ACTIVE device of
/// `user_id`. `false` means this device was revoked (or removed) and the caller
/// should log out. Authoritative + spoof-resistant: a forged `device_revoked`
/// inbox nudge cannot force a logout unless the device is genuinely gone.
/// Returns `true` when the device id isn't known yet (early boot) so it never
/// logs out a device that simply hasn't initialized.
///
/// The `revoked_at IS NULL` predicate is load-bearing (#685). Since #372
/// revocation is a TOMBSTONE, not a hard delete — the row is deliberately
/// retained so `verify_added_devices` can tell "revoked, drop rejoin" from
/// "lagging, keep commits", and so historical certs stay verifiable. A bare
/// existence check therefore returns `true` for a revoked device, which silently
/// disabled the entire graceful sign-out path: `revoke_device` publishes a
/// `device_revoked` nudge specifically so each device re-checks here and only
/// the revoked one logs out, and that check could never fail.
pub async fn is_current_device_registered(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<bool> {
    let device_id = match state.device_id.lock().await.clone() {
        Some(d) => d,
        None => return Ok(true),
    };
    // `include_revoked: false` IS the `revoked_at IS NULL` predicate, now
    // applied server-side (#987) — see the note above for why it is load-bearing.
    // Asking for one id keeps this a point lookup rather than a device-list scan.
    let rows = crate::commands::ds_reads::devices(
        state,
        Some(&user_id),
        vec![device_id.clone()],
        false,
    )
    .await?;
    Ok(rows.iter().any(|d| d.device_id == device_id))
}

/// The pre-unlock email → `user_id` resolution, after #997 moved the address
/// out of `accounts.json` and into the per-user keystore.
///
/// Everything here runs against a bare `AppState`: `local_db` is `None`,
/// `unlock` is `None`, `device_id` is `None`. That is not incidental — it is the
/// exact state the callers run in (no signer, no session, no DS credential), and
/// it is the whole reason a locally-resolvable address has to exist at all.
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

    /// Case-insensitivity is the property that made a masked address unusable
    /// here: `***@example.com` would compare equal for BOTH accounts below and
    /// resolve to an arbitrary one. Real addresses cannot collide, so the
    /// resolution stays exact even when two accounts share a domain.
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

    /// An account present in the index but with no keystore entry — a legacy
    /// install whose address is only in the pre-#997 `accounts.json`, or a
    /// keystore that failed to answer — must not stall the scan or resolve to
    /// the wrong account. It is skipped, and the account behind it still wins.
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

    /// The whole point of the move: the address lives in the keystore, and
    /// `AccountInfo` — everything `accounts.json` serializes — has no field that
    /// can hold it. This is the invalid state the change makes unrepresentable.
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

    /// The upgrade path. An install that predates #997 has the address on disk
    /// and nowhere else; after one migrated read it is in the keystore and the
    /// returning-device resolution works from there — which is the whole reason
    /// the move could not simply DROP the field.
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

    /// An index with nothing legacy in it must not authorize a rewrite — the
    /// erase step is for files that actually carry an address, and a launch that
    /// finds none should leave the file untouched.
    #[tokio::test]
    async fn an_index_with_no_legacy_address_authorizes_no_rewrite() {
        let state = locked_state();
        let accounts = vec![account("01USER_A", "alice")];

        assert!(!adopt_legacy_login_emails(&state, &accounts).await);
    }

    /// The keystore is the newer copy once the upgrade has run: a stale address
    /// still sitting in the file must not overwrite the one a later email change
    /// wrote to the keystore.
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

#[cfg(test)]
mod tests {
    use rusqlite::Connection;


    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        // The SHIPPED schema: baseline plus every numbered migration, in the order
        // `scripts/db-apply.sh` applies them. #875 — applying the baseline alone left
        // the fixture on a pre-migration schema no deploy has run for months.
        for sql in pollis_schema::main_scripts() {
            conn.execute_batch(sql).unwrap();
        }
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


/// The pure half of the account-creation contract. The DB-touching half lives in
/// `tests/auth_account_creation.rs`, which needs its own process (libsql's local
/// backend cannot initialise SQLite after rusqlite already has).
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
