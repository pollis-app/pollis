//! Sending one share to several conversations (#2100).
//!
//! A share is one draft (an optional message, and any files) sent to up to
//! [`MAX_TARGETS`](super::targets::MAX_TARGETS) conversations. Each target gets its own ordinary
//! message, sealed under that conversation's MLS group and its files under that conversation's
//! file key ring (#1382): nothing is reused across conversations, and nothing on the wire tells a
//! shared message from a typed one.
//!
//! Two halves, so nothing is lost or doubled:
//! - [`enqueue_share`] writes every target's outbox row (#1335), files included, before any
//!   network work. Each row's id is derived from the share's own id and the conversation
//!   ([`target_message_id`]), so running the same share again adopts the rows it already wrote
//!   instead of writing new ones, and the DS answers a re-posted envelope id as a replay.
//! - [`deliver_share`] delivers them. A process that stops before it finishes (the iOS
//!   extension dismissed or killed, the app sent to the background) leaves the rest in the
//!   outbox, which the app's next drain delivers, once.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ulid::Ulid;

use super::targets::MAX_TARGETS;
use crate::commands::link_preview::LinkPreviewDraft;
use crate::commands::messages::{EnqueueOutcome, OutgoingAttachment};
use crate::error::{Error, Result};
use crate::state::AppState;

/// Most items (files) one share may carry: the iOS activation rule and Android's
/// `SEND_MULTIPLE` both stop here. Together they stay under the outbox's per-message cap.
pub const MAX_ITEMS: usize = 10;

/// Longest a share waits for a link preview it builds itself before sending without one.
const PREVIEW_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// What a host hands over. Field names are the C ABI's and the bridge's contract.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareRequest {
    /// A ULID the host mints once per share and reuses on any retry of it.
    pub share_id: String,
    /// Conversation ids, 1 to [`MAX_TARGETS`], no repeats.
    pub targets: Vec<String>,
    /// The message: the user's text, which a shared text or link pre-filled. A caption when
    /// there are files.
    #[serde(default)]
    pub text: String,
    /// Files, each by `path` (never staged bytes); read from disk one chunk at a time.
    #[serde(default)]
    pub attachments: Vec<OutgoingAttachment>,
    /// A preview the host already built (`fetch_link_preview`). Without one, the share builds
    /// it for the message's first link itself, when previews are on.
    #[serde(default)]
    pub link_preview: Option<LinkPreviewDraft>,
}

/// One target's row in the outbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedShare {
    pub conversation_id: String,
    pub message_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareStatus {
    /// Delivered.
    Sent,
    /// Stored but not yet delivered: the app's outbox finishes it.
    Queued,
    /// Refused in a way no retry fixes (e.g. no longer a member); the chat shows "Not sent".
    Rejected,
}

/// How one target ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShareOutcome {
    pub conversation_id: String,
    pub message_id: String,
    pub status: ShareStatus,
}

/// The message id a share's copy for `conversation_id` gets: a ULID with the share's time and
/// 80 bits of `SHA-256(share id, conversation)`. The same share always names the same rows, two
/// targets never share one, and the id sorts like any other message sent at that moment.
pub fn target_message_id(share_id: &str, conversation_id: &str) -> Result<String> {
    let share = Ulid::from_string(share_id)
        .map_err(|_| Error::Other(anyhow::anyhow!("invalid share id")))?;
    let mut h = Sha256::new();
    h.update(b"pollis-share-target-v1");
    h.update((share_id.len() as u64).to_be_bytes());
    h.update(share_id.as_bytes());
    h.update(conversation_id.as_bytes());
    let digest = h.finalize();
    let mut random = [0u8; 16];
    random[6..].copy_from_slice(&digest[..10]);
    Ok(Ulid::from_parts(share.timestamp_ms(), u128::from_be_bytes(random)).to_string())
}

/// The rules a request must meet before anything is stored. Pure.
pub(crate) fn validate(req: &ShareRequest) -> Result<()> {
    let refuse = |why: &str| Err(Error::Other(anyhow::anyhow!("{why}")));
    if Ulid::from_string(&req.share_id).is_err() {
        return refuse("invalid share id");
    }
    if req.targets.is_empty() {
        return refuse("choose at least one chat");
    }
    if req.targets.len() > MAX_TARGETS {
        return refuse("a share goes to at most 5 chats");
    }
    let mut seen = std::collections::HashSet::new();
    if !req.targets.iter().all(|t| !t.is_empty() && seen.insert(t.as_str())) {
        return refuse("each chat may be chosen once");
    }
    if req.text.trim().is_empty() && req.attachments.is_empty() {
        return refuse("nothing to send");
    }
    if req.attachments.len() > MAX_ITEMS {
        return refuse("a share carries at most 10 items");
    }
    if req
        .attachments
        .iter()
        .any(|a| a.staged_id.is_some() || a.path.as_deref().is_none_or(str::is_empty))
    {
        return refuse("a shared file must be a path");
    }
    Ok(())
}

/// The first https link in `text` that a preview may be built for, as the composer picks it
/// (`previewCandidate`): a bare `www.` link counts as its `https://` form.
pub(crate) fn preview_candidate(text: &str) -> Option<String> {
    const LEADING: &str = "([<\"'";
    const TRAILING: &str = ".,;:!?)]>\"'";
    text.split_whitespace().find_map(|token| {
        let token = token
            .trim_start_matches(|c| LEADING.contains(c))
            .trim_end_matches(|c| TRAILING.contains(c));
        let candidate = if token.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://")) {
            token.to_string()
        } else if token.get(..4).is_some_and(|s| s.eq_ignore_ascii_case("www.")) {
            format!("https://{token}")
        } else {
            return None;
        };
        let url = url::Url::parse(&candidate).ok()?;
        let url = url.to_string();
        crate::commands::link_preview::wire::is_previewable_url(&url).then_some(url)
    })
}

/// The preview a share without one gets: built here for the message's first link, bounded by
/// [`PREVIEW_WAIT`]. `None` when there is no link, previews are off, or the page is slow.
async fn build_preview(state: &Arc<AppState>, text: &str) -> Option<LinkPreviewDraft> {
    let url = preview_candidate(text)?;
    let fetch = crate::commands::link_preview::fetch_link_preview(url, state);
    match tokio::time::timeout(PREVIEW_WAIT, fetch).await {
        Ok(Ok(draft)) => draft,
        Ok(Err(e)) => {
            crate::diag!("[share] link preview: {e}");
            None
        }
        Err(_) => {
            crate::diag!("[share] link preview: no answer in time; sending without one");
            None
        }
    }
}

/// Store every target's message (see the module docs). Refuses the whole share, storing
/// nothing, if any target is not one the user may share to. A failure part-way (a file that
/// vanished, a full disk) leaves the rows already written in the outbox; running the same share
/// again adopts them.
pub async fn enqueue_share(state: &Arc<AppState>, req: &ShareRequest) -> Result<Vec<QueuedShare>> {
    validate(req)?;
    let user_id = crate::commands::mls::current_user_id(state).await?;
    let boot = crate::commands::ds_reads::bootstrap(state, &user_id).await?;
    let refused = super::targets::ineligible(&user_id, &boot, &req.targets);
    if !refused.is_empty() {
        return Err(Error::Other(anyhow::anyhow!(
            "{} of the chosen chats can no longer be shared to",
            refused.len()
        )));
    }
    let draft = match &req.link_preview {
        Some(d) => Some(d.clone()),
        None => build_preview(state, &req.text).await,
    };
    let attachments: Vec<OutgoingAttachment> = req
        .attachments
        .iter()
        .cloned()
        .map(|mut a| {
            a.path = a.path.map(|p| p.strip_prefix("file://").unwrap_or(&p).to_string());
            a
        })
        .collect();
    let mut queued = Vec::with_capacity(req.targets.len());
    let mut result = Ok(());
    for conversation_id in &req.targets {
        let message_id = target_message_id(&req.share_id, conversation_id)?;
        let stored = crate::commands::messages::enqueue_message(
            Some(message_id.clone()),
            conversation_id.clone(),
            user_id.clone(),
            req.text.clone(),
            attachments.clone(),
            draft.as_ref(),
            None,
            None,
            state,
        )
        .await;
        match stored {
            Ok(EnqueueOutcome::Unsent(_) | EnqueueOutcome::AlreadySent(_)) => queued.push(QueuedShare {
                conversation_id: conversation_id.clone(),
                message_id,
            }),
            Err(e) => {
                result = Err(e);
                break;
            }
        }
    }
    // Every row holds its own copy of the thumbnail now.
    if let Some(d) = &draft {
        crate::commands::link_preview::release_draft(d);
    }
    result.map(|()| queued)
}

/// Deliver the rows [`enqueue_share`] wrote, every target at once, calling `progress` as each
/// ends. Never fails as a whole: a target the outbox could not deliver yet is
/// [`ShareStatus::Queued`] and stays in the outbox for the app to finish.
pub async fn deliver_share<F>(state: &Arc<AppState>, queued: &[QueuedShare], progress: F) -> Vec<ShareOutcome>
where
    F: Fn(&ShareOutcome) + Send + Sync,
{
    let progress = &progress;
    let one = |q: QueuedShare| async move {
        if let Err(e) = crate::commands::messages::deliver_queued_message(state, &q.message_id, None).await {
            crate::diag!("[share] delivery of {} not finished: {e}", q.message_id);
        }
        let status = match crate::commands::messages::send_state_of(state, &q.message_id).await {
            Ok(Some(None)) => ShareStatus::Sent,
            Ok(Some(Some(s))) if s == "rejected" => ShareStatus::Rejected,
            _ => ShareStatus::Queued,
        };
        let outcome = ShareOutcome {
            conversation_id: q.conversation_id,
            message_id: q.message_id,
            status,
        };
        progress(&outcome);
        outcome
    };
    futures_util::future::join_all(queued.iter().cloned().map(one)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(targets: &[&str], text: &str, files: usize) -> ShareRequest {
        ShareRequest {
            share_id: Ulid::new().to_string(),
            targets: targets.iter().map(|t| t.to_string()).collect(),
            text: text.into(),
            attachments: (0..files)
                .map(|i| OutgoingAttachment {
                    path: Some(format!("/tmp/f{i}")),
                    staged_id: None,
                    filename: format!("f{i}"),
                    content_type: "image/jpeg".into(),
                    blurhash: None,
                    width: None,
                    height: None,
                    sensitive: false,
                    poster: None,
                })
                .collect(),
            link_preview: None,
        }
    }

    #[test]
    fn a_share_names_the_same_rows_every_time_and_a_new_row_per_target() {
        let share = Ulid::new().to_string();
        let a = target_message_id(&share, "conv-a").unwrap();
        assert_eq!(a, target_message_id(&share, "conv-a").unwrap(), "stable across retries");
        let b = target_message_id(&share, "conv-b").unwrap();
        assert_ne!(a, b, "one row per target");
        let other = target_message_id(&Ulid::new().to_string(), "conv-a").unwrap();
        assert_ne!(a, other, "another share is another message");
        let parsed = Ulid::from_string(&a).expect("a ULID, as every message id is");
        assert_eq!(parsed.timestamp_ms(), Ulid::from_string(&share).unwrap().timestamp_ms());
        assert!(target_message_id("not-a-ulid", "conv-a").is_err());
    }

    #[test]
    fn invalid_requests_are_refused_before_anything_is_stored() {
        assert!(validate(&req(&["a"], "hi", 0)).is_ok());
        assert!(validate(&req(&["a", "b", "c", "d", "e"], "", 1)).is_ok());
        assert!(validate(&req(&[], "hi", 0)).is_err(), "no target");
        assert!(validate(&req(&["a", "b", "c", "d", "e", "f"], "hi", 0)).is_err(), "six targets");
        assert!(validate(&req(&["a", "a"], "hi", 0)).is_err(), "a repeat");
        assert!(validate(&req(&["a", ""], "hi", 0)).is_err(), "an empty id");
        assert!(validate(&req(&["a"], "  ", 0)).is_err(), "nothing to send");
        assert!(validate(&req(&["a"], "", 11)).is_err(), "eleven items");
        let mut staged = req(&["a"], "", 1);
        staged.attachments[0].path = None;
        staged.attachments[0].staged_id = Some("s".into());
        assert!(validate(&staged).is_err(), "staged bytes are not a share");
        let mut bad = req(&["a"], "hi", 0);
        bad.share_id = "nope".into();
        assert!(validate(&bad).is_err());
    }

    #[test]
    fn the_previewed_link_is_the_first_https_one() {
        assert_eq!(
            preview_candidate("see (https://example.com/a?b=1). and https://x.org").as_deref(),
            Some("https://example.com/a?b=1")
        );
        assert_eq!(preview_candidate("www.pollis.com rocks").as_deref(), Some("https://www.pollis.com/"));
        assert_eq!(preview_candidate("http://plain.example.com https://ok.example"), Some("https://ok.example/".into()));
        assert_eq!(preview_candidate("no links here"), None);
        assert_eq!(preview_candidate(""), None);
    }
}
