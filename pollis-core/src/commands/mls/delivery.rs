//! Client seam for submitting MLS commits to the log.
//!
//! Submission routes through the deployed Delivery Service, which is the *sole
//! writer* and serializes commits per conversation authoritatively (race-free,
//! gap-free, append-only — see the `pollis-delivery` crate). The DS writes the
//! commit + its resulting-epoch GroupInfo + the added members' Welcomes
//! atomically on a win, returning a [`SubmitResult`].
//!
//! Scope: the commit, its resulting-epoch GroupInfo, and the added members'
//! Welcomes land as ONE unit through this seam. `submit_commit` OWNS all three —
//! the committer no longer writes Welcomes inline or republishes GroupInfo after
//! the merge.

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
