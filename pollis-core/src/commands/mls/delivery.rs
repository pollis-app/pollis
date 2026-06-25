//! Client seam for submitting MLS commits to the log.
//!
//! Two paths, one decision, keyed on `config.pollis_delivery_url`
//! (compile-time-baked from `POLLIS_DELIVERY_URL`, or runtime env in dev):
//! - **Direct** (default, when it's `None`): the client writes the commit +
//!   GroupInfo + Welcomes straight to Turso — mirroring what the Delivery
//!   Service does on a win, so the test/local path is byte-for-byte equivalent
//!   to the DS path. Used in tests and until the Delivery Service is deployed.
//! - **Http** (when it's `Some(url)`): submission routes through the deployed
//!   Delivery Service, which becomes the *sole writer* and serializes commits
//!   per conversation authoritatively (race-free, gap-free, append-only — see
//!   the `pollis-delivery` crate). The DS writes commit + GroupInfo + Welcomes
//!   atomically on a win.
//!
//! Either way the caller gets the same [`SubmitResult`] and doesn't care which
//! path ran — that's the whole point of the seam.
//!
//! Scope (Slice 1): the commit, its resulting-epoch GroupInfo, and the added
//! members' Welcomes now land as ONE unit through this seam. `submit_commit`
//! OWNS all three — the committer no longer writes Welcomes inline or
//! republishes GroupInfo after the merge.

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::state::AppState;

pub enum SubmitResult {
    /// Our commit won its epoch.
    Committed,
    /// Someone else committed this epoch first; the caller must converge on the
    /// winner (roll back its local pending commit and re-process).
    LostRace,
}

/// One Welcome destined for a device added by this commit. The recipient
/// (user_id + device_id) plus the TLS-serialized MLS Welcome blob. Mirrors the
/// DS `WelcomeBody`.
pub struct WelcomeOut {
    pub recipient_id: String,
    pub recipient_device_id: String,
    pub welcome: Vec<u8>,
}

/// Submit one commit at `epoch` for `conversation_id`, together with the
/// resulting-epoch `group_info` (if any) and the `welcomes` for any devices the
/// commit added. On a win, all three are written as one unit (atomically by the
/// DS; mirrored by the Direct path). See the module docs.
pub async fn submit_commit(
    state: &Arc<AppState>,
    conversation_id: &str,
    epoch: i64,
    sender_id: &str,
    commit: &[u8],
    added_user_id: Option<&str>,
    added_device_ids: Option<&str>,
    group_info: Option<&[u8]>,
    welcomes: &[WelcomeOut],
) -> Result<SubmitResult> {
    match state.config.pollis_delivery_url.as_deref() {
        Some(_) => {
            http_submit(
                state,
                conversation_id,
                epoch,
                sender_id,
                commit,
                added_user_id,
                added_device_ids,
                group_info,
                welcomes,
            )
            .await
        }
        None => {
            direct_submit(
                state,
                conversation_id,
                epoch,
                sender_id,
                commit,
                added_user_id,
                added_device_ids,
                group_info,
                welcomes,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn direct_submit(
    state: &Arc<AppState>,
    conversation_id: &str,
    epoch: i64,
    sender_id: &str,
    commit: &[u8],
    added_user_id: Option<&str>,
    added_device_ids: Option<&str>,
    group_info: Option<&[u8]>,
    welcomes: &[WelcomeOut],
) -> Result<SubmitResult> {
    let conn = state.remote_db.conn().await?;
    // Claim this epoch. The UNIQUE(conversation_id, epoch) constraint +
    // `ON CONFLICT DO NOTHING` makes exactly one writer win per epoch (no fork),
    // matching the prior Direct behavior so existing race tests stay green.
    let affected = conn
        .execute(
            "INSERT INTO mls_commit_log \
             (conversation_id, epoch, sender_id, commit_data, added_user_id, added_device_ids) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(conversation_id, epoch) DO NOTHING",
            libsql::params![
                conversation_id.to_string(),
                epoch,
                sender_id.to_string(),
                commit.to_vec(),
                added_user_id.map(str::to_string),
                added_device_ids.map(str::to_string),
            ],
        )
        .await?;

    if affected == 0 {
        return Ok(SubmitResult::LostRace);
    }

    // Won the epoch. Write the resulting-epoch GroupInfo + any Welcomes so a
    // future joiner / newly-added device can come online — exactly mirroring
    // `pollis-delivery/src/commit.rs` on a win (same SQL, same semantics). This
    // keeps the Direct/test path equivalent to the DS path.
    if let Some(gi) = group_info {
        conn.execute(
            "INSERT INTO mls_group_info (conversation_id, epoch, group_info, updated_by_device_id) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(conversation_id) DO UPDATE SET \
                 epoch = excluded.epoch, \
                 group_info = excluded.group_info, \
                 updated_by_device_id = excluded.updated_by_device_id, \
                 updated_at = datetime('now')",
            libsql::params![
                conversation_id.to_string(),
                epoch + 1,
                gi.to_vec(),
                sender_id.to_string(),
            ],
        )
        .await?;
    }
    for w in welcomes {
        conn.execute(
            "INSERT INTO mls_welcome \
                 (id, conversation_id, recipient_id, welcome_data, recipient_device_id) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            libsql::params![
                ulid::Ulid::new().to_string(),
                conversation_id.to_string(),
                w.recipient_id.clone(),
                w.welcome.clone(),
                w.recipient_device_id.clone(),
            ],
        )
        .await?;
    }

    Ok(SubmitResult::Committed)
}

#[allow(clippy::too_many_arguments)]
async fn http_submit(
    state: &Arc<AppState>,
    conversation_id: &str,
    epoch: i64,
    sender_id: &str,
    commit: &[u8],
    added_user_id: Option<&str>,
    added_device_ids: Option<&str>,
    group_info: Option<&[u8]>,
    welcomes: &[WelcomeOut],
) -> Result<SubmitResult> {
    use base64::Engine as _;
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let welcomes_json: Vec<serde_json::Value> = welcomes
        .iter()
        .map(|w| {
            serde_json::json!({
                "recipient_id": w.recipient_id,
                "recipient_device_id": w.recipient_device_id,
                "welcome": b64(&w.welcome),
            })
        })
        .collect();
    let body = serde_json::json!({
        "conversation_id": conversation_id,
        "based_on_epoch": epoch,
        "sender_id": sender_id,
        "commit": b64(commit),
        "added_user_id": added_user_id,
        "added_device_ids": added_device_ids,
        "group_info": group_info.map(b64),
        "welcomes": welcomes_json,
    });
    // Signed POST: attaches the four `X-Pollis-*` auth headers. When the DS has
    // auth disabled the headers are ignored, so behavior is identical to the
    // previous unsigned submit.
    let resp = super::ds_client::ds_post(state, "/v1/commits", &body).await?;
    match resp.status() {
        s if s.is_success() => Ok(SubmitResult::Committed),
        reqwest::StatusCode::CONFLICT => Ok(SubmitResult::LostRace),
        s => {
            let txt = resp.text().await.unwrap_or_default();
            Err(Error::Other(anyhow::anyhow!("delivery submit {s}: {txt}")))
        }
    }
}
