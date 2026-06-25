//! Client seam for submitting MLS commits to the log.
//!
//! Two paths, one decision, keyed on `config.pollis_delivery_url`
//! (compile-time-baked from `POLLIS_DELIVERY_URL`, or runtime env in dev):
//! - **Direct** (default, when it's `None`): the client
//!   writes the commit straight to `mls_commit_log` — byte-for-byte the prior
//!   behavior. Used in tests and until the Delivery Service is deployed.
//! - **Http** (when it's `Some(url)`): submission routes through the
//!   deployed Delivery Service, which becomes the *sole writer* and serializes
//!   commits per conversation authoritatively (race-free, gap-free, append-only
//!   — see the `pollis-delivery` crate).
//!
//! Either way the caller gets the same [`SubmitResult`] and doesn't care which
//! path ran — that's the whole point of the seam.
//!
//! Scope (this step): the **commit-log write only**. Welcomes / GroupInfo
//! writes still go direct for now (they move behind the DS later), and the
//! gap-preventing head check lives in the DS — the Direct path keeps the prior
//! plain `ON CONFLICT` semantics so this change is behavior-identical.

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

/// Submit one commit at `epoch` for `conversation_id`. See the module docs.
pub async fn submit_commit(
    state: &Arc<AppState>,
    conversation_id: &str,
    epoch: i64,
    sender_id: &str,
    commit: &[u8],
    added_user_id: Option<&str>,
    added_device_ids: Option<&str>,
) -> Result<SubmitResult> {
    match state.config.pollis_delivery_url.as_deref() {
        Some(base) => {
            http_submit(base, conversation_id, epoch, sender_id, commit, added_user_id, added_device_ids).await
        }
        None => {
            direct_submit(state, conversation_id, epoch, sender_id, commit, added_user_id, added_device_ids).await
        }
    }
}

async fn direct_submit(
    state: &Arc<AppState>,
    conversation_id: &str,
    epoch: i64,
    sender_id: &str,
    commit: &[u8],
    added_user_id: Option<&str>,
    added_device_ids: Option<&str>,
) -> Result<SubmitResult> {
    let conn = state.remote_db.conn().await?;
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
    Ok(if affected == 0 {
        SubmitResult::LostRace
    } else {
        SubmitResult::Committed
    })
}

async fn http_submit(
    base_url: &str,
    conversation_id: &str,
    epoch: i64,
    sender_id: &str,
    commit: &[u8],
    added_user_id: Option<&str>,
    added_device_ids: Option<&str>,
) -> Result<SubmitResult> {
    use base64::Engine as _;
    let body = serde_json::json!({
        "conversation_id": conversation_id,
        "based_on_epoch": epoch,
        "sender_id": sender_id,
        "commit": base64::engine::general_purpose::STANDARD.encode(commit),
        "added_user_id": added_user_id,
        "added_device_ids": added_device_ids,
    });
    let url = format!("{}/v1/commits", base_url.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("delivery submit: {e}")))?;
    match resp.status() {
        s if s.is_success() => Ok(SubmitResult::Committed),
        reqwest::StatusCode::CONFLICT => Ok(SubmitResult::LostRace),
        s => {
            let txt = resp.text().await.unwrap_or_default();
            Err(Error::Other(anyhow::anyhow!("delivery submit {s}: {txt}")))
        }
    }
}
