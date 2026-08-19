//! The client half of the Delivery Service's directory and account READ
//! endpoints (#987).
//!
//! Sibling of `commands::mls::ds_reads`, which owns the MLS control-plane reads.
//! Split by domain rather than merged, because the MLS ones carry correctness
//! constraints (one snapshot, contiguous batch, opposite failure biases) that
//! nothing here does — putting them in one module would invite a helper that
//! quietly applies one module's rules to the other's callers.
//!
//! Everything here is a thin typed wrapper: build the body, `ds_post_json`,
//! hand back the response. Deliberately no defaults and no error swallowing —
//! each caller keeps whatever bias it had when the read was a `SELECT`, because
//! those biases were chosen per call site and HTTP makes failures common where
//! libsql failures were rare.

use std::sync::Arc;

use pollis_api::account_reads as ar;
use pollis_api::directory as dir;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::mls::ds_client::{current_user_id, ds_post_json};

/// Decode one base64 blob from a read response.
pub(crate) fn decode_b64(what: &str, s: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| Error::Other(anyhow::anyhow!("{what}: invalid base64 from DS: {e}")))
}

// ── Conversations ────────────────────────────────────────────────────────────

/// Resolve a conversation to its MLS group and enumerate what that group backs.
///
/// The chain this replaces was five DEPENDENT round trips — resolve, membership,
/// is-DM, channel enumerate, envelope fetch — each needing the previous answer,
/// so none of them could be pipelined. `want_envelopes` is opt-in because the
/// several callers that need only the resolution should not pay for the scan.
pub(crate) async fn catch_up(
    state: &Arc<AppState>,
    conversation_id: &str,
    want_envelopes: bool,
    want_dm: bool,
) -> Result<dir::CatchUpResponse> {
    let body = dir::CatchUpBody {
        conversation_id: conversation_id.to_string(),
        want_envelopes,
        want_dm,
        user_id: Some(current_user_id(state).await?),
        device_id: state.device_id.lock().await.clone(),
    };
    ds_post_json(state, &body).await
}

/// Which conversation an envelope belongs to, or `None`.
///
/// `None` covers both "no such envelope" and "not a member of its conversation",
/// deliberately: distinguishing them would make this an existence oracle for
/// message ids, and the caller cannot act on either answer.
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
///
/// Batched because every caller has a page of messages, not one: the per-message
/// read this replaces meant one round trip per row on screen.
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

// ── Directory ────────────────────────────────────────────────────────────────

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
    };
    ds_post_json(state, &body).await
}

/// Existence and the caller's role, and nothing else — the authorization
/// preflights' read.
///
/// A roster is not free, and the preflights want one string. `group_id` may name
/// a channel, in which case the answer is about its owning group.
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
///
/// Direction-blind, because every caller halts delivery on either side's block
/// and reports the same generic refusal — so neither party can infer which of
/// them blocked whom. Learning the direction here would put that inference one
/// debug log away from leaking.
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
    };
    ds_post_json(state, &body).await
}

// ── Accounts, devices, key packages ──────────────────────────────────────────

/// Cross-signing roots for a batch of users, keyed by user id.
///
/// A user absent from the result has no row or a NULL `account_id_pub` — "not
/// provisioned yet", which every caller reads as "do not pin" rather than as an
/// error.
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
pub(crate) async fn account_status(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<Option<ar::AccountStatus>> {
    let body = ar::AccountStatusBody {
        user_id: Some(user_id.to_string()),
    };
    Ok(ds_post_json(state, &body).await?.account)
}

/// This device's remaining unclaimed key packages in one suite.
pub(crate) async fn unclaimed_key_packages(
    state: &Arc<AppState>,
    ciphersuite: i64,
) -> Result<i64> {
    let body = ar::KeyPackagesBody {
        count_for_ciphersuite: Some(ciphersuite),
        pairs_for_user_ids: Vec::new(),
        pairs_ciphersuite: None,
    };
    Ok(ds_post_json(state, &body).await?.unclaimed.unwrap_or(0))
}

/// `(user_id, device_id)` pairs among `user_ids` with a claimable package.
///
/// A short list here is indistinguishable from "no package", which is how a
/// device silently stays out of a group — so this propagates the error rather
/// than degrading, exactly as the direct read's `with_retry` did.
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

// ── Objects ──────────────────────────────────────────────────────────────────

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
