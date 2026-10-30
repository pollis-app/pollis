//! The client half of the Delivery Service's directory and account READ
//! endpoints (#987).
//!
//! MLS control-plane reads live in `commands::mls::ds_reads`, which has stricter rules.
//!
//! Thin typed wrappers with no defaults or error swallowing; each caller keeps its own bias.

use std::sync::Arc;

use pollis_api::account_reads as ar;
use pollis_api::directory as dir;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::mls::ds_client::{current_user_id, ds_post_json};

/// Does this account exist, and does it have an identity yet?
/// Unauthenticated (#987 §6.5): runs before PIN entry, when no credential exists yet.
pub(crate) async fn account_probe(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<ar::AccountProbeResponse> {
    let body = ar::AccountProbeBody {
        user_id: user_id.to_string(),
    };
    let resp = crate::commands::mls::ds_post_plain(state, &body).await?;
    let status = resp.status();
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!(
            "account-probe {status}: {txt}"
        )));
    }
    crate::commands::mls::decode_response::<ar::AccountProbeBody>(resp).await
}

/// Decode one base64 blob from a read response.
pub(crate) fn decode_b64(what: &str, s: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| Error::Other(anyhow::anyhow!("{what}: invalid base64 from DS: {e}")))
}

/// Resolve a conversation to its MLS group and enumerate what that group backs.
pub(crate) async fn catch_up(
    state: &Arc<AppState>,
    conversation_id: &str,
    want_envelopes: bool,
    want_dm: bool,
) -> Result<dir::CatchUpResponse> {
    catch_up_full(state, conversation_id, want_envelopes, want_dm, false).await
}

/// [`catch_up`], with the MLS roster as well.
pub(crate) async fn catch_up_full(
    state: &Arc<AppState>,
    conversation_id: &str,
    want_envelopes: bool,
    want_dm: bool,
    want_roster: bool,
) -> Result<dir::CatchUpResponse> {
    let body = dir::CatchUpBody {
        conversation_id: conversation_id.to_string(),
        want_envelopes,
        want_dm,
        want_roster,
        user_id: Some(current_user_id(state).await?),
        device_id: state.device_id.lock().await.clone(),
    };
    ds_post_json(state, &body).await
}

/// Which conversation an envelope belongs to, or `None`.
/// `None` also for non-members, so this is no existence oracle for message ids.
pub(crate) async fn message_conversation(
    state: &Arc<AppState>,
    message_id: &str,
) -> Result<Option<String>> {
    let body = dir::MessageLookupBody {
        message_id: message_id.to_string(),
        user_id: Some(current_user_id(state).await?),
        reactions_for: Vec::new(),
    };
    Ok(ds_post_json(state, &body).await?.conversation_id)
}

/// Reactions on a batch of messages, keyed by message id.
pub(crate) async fn message_reactions(
    state: &Arc<AppState>,
    message_ids: Vec<String>,
) -> Result<Vec<dir::MessageReactions>> {
    if message_ids.is_empty() {
        return Ok(Vec::new());
    }
    let body = dir::MessageLookupBody {
        message_id: String::new(),
        user_id: Some(current_user_id(state).await?),
        reactions_for: message_ids,
    };
    Ok(ds_post_json(state, &body).await?.reactions)
}

pub(crate) async fn bootstrap(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<dir::DirectoryBootstrapResponse> {
    let body = dir::DirectoryBootstrapBody {
        user_id: Some(user_id.to_string()),
    };
    ds_post_json(state, &body).await
}

/// One group's detail, gated per section server-side. `group_id` may also name
/// a CHANNEL, which the DS resolves to its owning group.
pub(crate) async fn group(
    state: &Arc<AppState>,
    group_id: &str,
    user_id: &str,
    want_emoji: bool,
) -> Result<dir::DirectoryGroupResponse> {
    let body = dir::DirectoryGroupBody {
        group_id: group_id.to_string(),
        user_id: Some(user_id.to_string()),
        role_only: false,
        want_emoji,
        want_invites: false,
    };
    ds_post_json(state, &body).await
}

/// One group's detail with its pending invites (#1506) — the member page's read.
pub(crate) async fn group_with_invites(
    state: &Arc<AppState>,
    group_id: &str,
    user_id: &str,
) -> Result<dir::DirectoryGroupResponse> {
    let body = dir::DirectoryGroupBody {
        group_id: group_id.to_string(),
        user_id: Some(user_id.to_string()),
        role_only: false,
        want_emoji: false,
        want_invites: true,
    };
    ds_post_json(state, &body).await
}

/// Existence and the caller's role, and nothing else — the authorization
/// preflights' read.
pub(crate) async fn group_role(
    state: &Arc<AppState>,
    group_id: &str,
    user_id: &str,
) -> Result<dir::DirectoryGroupResponse> {
    let body = dir::DirectoryGroupBody {
        group_id: group_id.to_string(),
        user_id: Some(user_id.to_string()),
        role_only: true,
        want_emoji: false,
        want_invites: false,
    };
    ds_post_json(state, &body).await
}

pub(crate) async fn users(
    state: &Arc<AppState>,
    user_ids: Vec<String>,
    identifier: Option<String>,
) -> Result<Vec<dir::UserWire>> {
    if user_ids.is_empty() && identifier.is_none() {
        return Ok(Vec::new());
    }
    let body = dir::DirectoryUsersBody {
        user_ids,
        identifier,
        block_check: Vec::new(),
    };
    Ok(ds_post_json(state, &body).await?.users)
}

/// Which of `candidates` are in a block relationship with the caller, in either
/// direction.
/// Direction-blind, so neither party can infer who blocked whom.
pub(crate) async fn blocked_among(
    state: &Arc<AppState>,
    candidates: &[String],
) -> Result<Vec<String>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let body = dir::DirectoryUsersBody {
        user_ids: Vec::new(),
        identifier: None,
        block_check: candidates.to_vec(),
    };
    Ok(ds_post_json(state, &body).await?.blocked)
}

pub(crate) async fn conversations(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<dir::DirectoryConversationsResponse> {
    let body = dir::DirectoryConversationsBody {
        user_id: Some(user_id.to_string()),
        dm_with_user_id: None,
    };
    ds_post_json(state, &body).await
}

/// The 1:1 DM channel the caller shares with `other_user_id`, if any.
/// The DS scopes the search to the caller's own DMs.
#[cfg(feature = "media")]
pub(crate) async fn one_to_one_dm(
    state: &Arc<AppState>,
    user_id: &str,
    other_user_id: &str,
) -> Result<Option<String>> {
    let body = dir::DirectoryConversationsBody {
        user_id: Some(user_id.to_string()),
        dm_with_user_id: Some(other_user_id.to_string()),
    };
    Ok(ds_post_json(state, &body).await?.dm_with)
}

/// Cross-signing roots for a batch of users, keyed by user id.
/// Absent users are "not provisioned yet": callers don't pin, not an error.
pub(crate) async fn account_keys(
    state: &Arc<AppState>,
    user_ids: &[String],
) -> Result<Vec<(String, Vec<u8>, i64)>> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    let body = ar::AccountKeysBody {
        user_ids: user_ids.to_vec(),
    };
    let resp = ds_post_json(state, &body).await?;
    resp.keys
        .into_iter()
        .map(|k| {
            let bytes = decode_b64("account_id_pub", &k.account_id_pub)?;
            Ok((k.user_id, bytes, k.identity_version))
        })
        .collect()
}

/// One user's `account_id_pub` and identity version, or an error when they have
/// none — the shape the safety-number path expects.
pub(crate) async fn account_key(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<(Vec<u8>, i64)> {
    let mut keys = account_keys(state, std::slice::from_ref(&user_id.to_string())).await?;
    if keys.is_empty() {
        return Err(Error::Other(anyhow::anyhow!(
            "no account_id_pub for user {user_id}"
        )));
    }
    let (_, key, version) = keys.remove(0);
    Ok((key, version))
}

pub(crate) async fn devices(
    state: &Arc<AppState>,
    user_id: Option<&str>,
    device_ids: Vec<String>,
    include_revoked: bool,
) -> Result<Vec<ar::DeviceRow>> {
    let body = ar::DevicesBody {
        user_id: user_id.map(str::to_string),
        device_ids,
        include_revoked,
    };
    Ok(ds_post_json(state, &body).await?.devices)
}

/// The authenticated user's own account row.
///
/// Device-signed once this device can sign; before that (a fresh device on the
/// soft-reset path, #1616) the verified-OTP session authenticates it instead.
pub(crate) async fn account_status(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<Option<ar::AccountStatus>> {
    let body = ar::AccountStatusBody {
        user_id: Some(user_id.to_string()),
    };
    Ok(crate::commands::mls::ds_post_signed_or_session_json(state, &body)
        .await?
        .account)
}

/// This device's remaining unclaimed (regular) key packages in one suite, and
/// whether it holds a last-resort package there (#1571; `None` from a Delivery
/// Service that predates them).
pub(crate) async fn unclaimed_key_packages(
    state: &Arc<AppState>,
    ciphersuite: i64,
) -> Result<(i64, Option<bool>)> {
    let body = ar::KeyPackagesBody {
        count_for_ciphersuite: Some(ciphersuite),
        pairs_for_user_ids: Vec::new(),
        pairs_ciphersuite: None,
    };
    let resp = ds_post_json(state, &body).await?;
    Ok((resp.unclaimed.unwrap_or(0), resp.has_last_resort))
}

/// `(user_id, device_id)` pairs among `user_ids` with a claimable package —
/// an unclaimed regular one, or (#1571) a last-resort one, which is never
/// claimed and so keeps a device with an empty pool on this list.
/// Propagates errors: a short list would silently keep a device out of a group.
pub(crate) async fn claimable_devices(
    state: &Arc<AppState>,
    user_ids: &[String],
    ciphersuite: Option<i64>,
) -> Result<Vec<(String, String)>> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    let body = ar::KeyPackagesBody {
        count_for_ciphersuite: None,
        pairs_for_user_ids: user_ids.to_vec(),
        pairs_ciphersuite: ciphersuite,
    };
    Ok(ds_post_json(state, &body)
        .await?
        .pairs
        .into_iter()
        .map(|p| (p.user_id, p.device_id))
        .collect())
}

/// Every non-revoked `(user, device)` pair still registered for a roster.
pub(crate) async fn registered_devices(
    state: &Arc<AppState>,
    user_ids: &[String],
) -> Result<std::collections::HashSet<(String, String)>> {
    if user_ids.is_empty() {
        return Ok(Default::default());
    }
    let body = ar::RegisteredDevicesBody {
        user_ids: user_ids.to_vec(),
    };
    Ok(ds_post_json(state, &body)
        .await?
        .pairs
        .into_iter()
        .map(|p| (p.user_id, p.device_id))
        .collect())
}

/// Every roster user's `account_id_pub` and ALL of their `user_device` rows
/// (revoked included) — the inputs to the committer-side leaf cross-signing
/// check (`mls::device::IdentityDirectory`).
/// Errors rather than truncating: a short answer would refuse or drop live devices.
pub(crate) async fn roster_identities(
    state: &Arc<AppState>,
    user_ids: &[String],
) -> Result<Vec<pollis_api::reads::AddedIdentity>> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    let body = ar::RosterIdentitiesBody {
        user_ids: user_ids.to_vec(),
    };
    Ok(ds_post_json(state, &body).await?.identities)
}

/// One device-enrollment request, addressed by its id.
/// Unsigned unless `want_approval_fields`: the poller has no credential; the 128-bit id is the gate.
pub(crate) async fn enrollment_request(
    state: &Arc<AppState>,
    request_id: &str,
    want_approval_fields: bool,
) -> Result<Option<ar::EnrollmentRequestRow>> {
    let body = ar::EnrollmentRequestBody {
        request_id: request_id.to_string(),
        want_approval_fields,
    };
    let resp = if want_approval_fields {
        crate::commands::mls::ds_post(state, &body).await?
    } else {
        crate::commands::mls::ds_post_plain(state, &body).await?
    };
    let status = resp.status();
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!("read/enrollment {status}: {txt}")));
    }
    Ok(crate::commands::mls::decode_response::<ar::EnrollmentRequestBody>(resp)
        .await?
        .request)
}

/// The authenticated user's still-open enrollment requests, newest first.
pub(crate) async fn pending_enrollments(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<Vec<ar::EnrollmentRequestRow>> {
    let body = ar::PendingEnrollmentsBody {
        user_id: Some(user_id.to_string()),
    };
    Ok(ds_post_json(state, &body).await?.requests)
}

/// The Secret-Key-wrapped account identity: `(salt, nonce, wrapped_key)`.
/// OTP-session credential: a recovering device has nothing else.
pub(crate) async fn recovery_blob(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<Option<(Vec<u8>, Vec<u8>, Vec<u8>)>> {
    let body = ar::RecoveryBlobBody {
        user_id: user_id.to_string(),
    };
    let resp = crate::commands::mls::ds_post_signed_or_session(state, &body).await?;
    let status = resp.status();
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!(
            "read/recovery-blob {status}: {txt}"
        )));
    }
    let blob = crate::commands::mls::decode_response::<ar::RecoveryBlobBody>(resp)
        .await?
        .blob;
    match blob {
        Some(b) => Ok(Some((
            decode_b64("recovery salt", &b.salt)?,
            decode_b64("recovery nonce", &b.nonce)?,
            decode_b64("recovery wrapped_key", &b.wrapped_key)?,
        ))),
        None => Ok(None),
    }
}

/// One keyset page of the account's own security-event log, most recent first
/// (#1548). `before`/`kinds` unset = the pre-#1548 body, byte for byte.
pub(crate) async fn security_events(
    state: &Arc<AppState>,
    user_id: &str,
    limit: i64,
    before: Option<ar::SecurityEventCursor>,
    kinds: Option<Vec<String>>,
) -> Result<ar::SecurityEventsResponse> {
    let body = ar::SecurityEventsBody {
        user_id: Some(user_id.to_string()),
        limit: Some(limit),
        before,
        kinds,
    };
    ds_post_json(state, &body).await
}

/// This account's synced read-cursor blob, or `None` when nothing has ever been
/// pushed.
/// No defaulting: a decode failure must not re-badge everything as unread.
pub(crate) async fn read_cursors(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<ar::ReadCursorsResponse> {
    let body = ar::ReadCursorsBody {
        user_id: user_id.to_string(),
    };
    ds_post_json(state, &body).await
}

/// The channels this account muted (#1431). No defaulting: a failure surfaces,
/// so a caller never mistakes "could not ask" for "nothing is muted".
pub(crate) async fn channel_mutes(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<pollis_api::profile::ChannelMutesResponse> {
    let body = pollis_api::profile::ChannelMutesBody {
        user_id: user_id.to_string(),
    };
    ds_post_json(state, &body).await
}

/// The conversation's current wrapped pin key, or `None` when nobody has
/// minted one. Deliberately no defaulting: a decode failure surfaces rather
/// than reading as "no pins here".
pub(crate) async fn read_pin_keystate(
    state: &Arc<AppState>,
    mls_conversation_id: &str,
    user_id: &str,
) -> Result<pollis_api::pins::PinKeystateResponse> {
    let body = pollis_api::pins::PinKeystateBody {
        conversation_id: mls_conversation_id.to_string(),
        user_id: Some(user_id.to_string()),
    };
    ds_post_json(state, &body).await
}

/// One conversation's pins, newest first, ciphertext and all.
pub(crate) async fn read_pins(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
) -> Result<pollis_api::pins::ListPinsResponse> {
    let body = pollis_api::pins::ListPinsBody {
        conversation_id: conversation_id.to_string(),
        user_id: Some(user_id.to_string()),
    };
    ds_post_json(state, &body).await
}

/// The conversation's current wrapped file key ring, or `None` when nobody has
/// minted one.
pub(crate) async fn read_file_keystate(
    state: &Arc<AppState>,
    mls_conversation_id: &str,
    user_id: &str,
) -> Result<pollis_api::file_keys::FileKeystateResponse> {
    let body = pollis_api::file_keys::FileKeystateBody {
        conversation_id: mls_conversation_id.to_string(),
        user_id: Some(user_id.to_string()),
    };
    ds_post_json(state, &body).await
}

/// Every vault entry this account holds, as the DS stores them — opaque until
/// `commands::vault` opens them under the account identity key.
pub(crate) async fn read_vault(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<pollis_api::vault::VaultMessagesResponse> {
    let body = pollis_api::vault::VaultMessagesBody {
        user_id: user_id.to_string(),
    };
    ds_post_json(state, &body).await
}

pub(crate) async fn object_exists(
    state: &Arc<AppState>,
    content_hash: &str,
    kind: ar::ObjectKind,
) -> Result<bool> {
    let body = ar::ObjectExistsBody {
        content_hash: content_hash.to_string(),
        kind,
    };
    Ok(ds_post_json(state, &body).await?.exists)
}
