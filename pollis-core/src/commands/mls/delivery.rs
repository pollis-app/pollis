//! Client seam for submitting MLS commits to the log.
//!
//! Routes through the Delivery Service, the sole writer, which stores commit, GroupInfo and Welcomes atomically.

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

/// One Welcome for a device this commit added; mirrors the DS `WelcomeBody`.
pub struct WelcomeOut {
    pub recipient_id: String,
    pub recipient_device_id: String,
    pub welcome: Vec<u8>,
}

/// Submit one commit plus its resulting GroupInfo and Welcomes, written as one unit on a win.
///
/// `closes_epoch` is set only by a migration opening generation N+1 (#454): the head of N it closes.
#[allow(clippy::too_many_arguments)]
pub async fn submit_commit(
    state: &Arc<AppState>,
    conversation_id: &str,
    generation: i64,
    epoch: i64,
    closes_epoch: Option<i64>,
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
        generation,
        epoch,
        closes_epoch,
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
    generation: i64,
    epoch: i64,
    closes_epoch: Option<i64>,
    sender_id: &str,
    commit: &[u8],
    added_user_id: Option<&str>,
    added_device_ids: Option<&str>,
    group_info: Option<&[u8]>,
    welcomes: &[WelcomeOut],
) -> Result<SubmitResult> {
    use crate::util::b64;
    let welcomes_json: Vec<pollis_api::commit::WelcomeBody> = welcomes
        .iter()
        .map(|w| pollis_api::commit::WelcomeBody {
            recipient_id: w.recipient_id.clone(),
            recipient_device_id: w.recipient_device_id.clone(),
            welcome: b64(&w.welcome),
        })
        .collect();
    let body = pollis_api::commit::SubmitBody {
        conversation_id: conversation_id.to_string(),
        generation,
        based_on_epoch: epoch,
        closes_epoch,
        sender_id: sender_id.to_string(),
        commit: b64(commit),
        added_user_id: added_user_id.map(str::to_string),
        added_device_ids: added_device_ids.map(str::to_string),
        group_info: group_info.map(b64),
        welcomes: welcomes_json,
    };
    let resp = super::ds_client::ds_post(state, &body).await?;
    match resp.status() {
        s if s.is_success() => Ok(SubmitResult::Committed),
        reqwest::StatusCode::CONFLICT => Ok(SubmitResult::LostRace),
        s => {
            let txt = resp.text().await.unwrap_or_default();
            Err(Error::Other(anyhow::anyhow!("delivery submit {s}: {txt}")))
        }
    }
}
