use std::sync::Arc;
use ulid::Ulid;

use crate::error::Result;
use crate::state::AppState;

use super::outbox::{OutgoingAttachment, Queued};
use super::types::Message;

/// Placeholder in the NOT NULL `message_envelope.sender_id` column of every
/// sealed envelope (#331); the true sender is the MLS credential inside the
/// ciphertext (`docs/metadata-minimization-design.md` §2.1).
pub const SEALED_SENDER_SENTINEL: &str = "sealed";

/// Send a message under a fresh id. See [`send_message_with_id`].
pub async fn send_message(
    conversation_id: String,
    sender_id: String,
    content: String,
    reply_to_id: Option<String>,
    // Thread root (#825). Travels only inside the MLS ciphertext, never in the
    // DS request body.
    thread_id: Option<String>,
    sender_username: Option<String>,
    state: &Arc<AppState>,
) -> Result<Message> {
    send_message_with_id(
        None,
        conversation_id,
        sender_id,
        content,
        Vec::new(),
        reply_to_id,
        thread_id,
        sender_username,
        state,
    )
    .await
}

/// Send a message, durably (#1335).
///
/// The local row is written as `send_state = 'pending'` before any network
/// work; that row is the outbox entry, and only a confirmed post clears it. A
/// message the user saw accepted is never silently gone.
///
/// `message_id` (from `new_message_id`) makes the renderer's optimistic row and
/// the stored row the same message; a repeat call with it is idempotent.
/// `attachments` makes the outbox own the upload; with none, `content` is the
/// finished envelope (older renderers, the TUI).
#[allow(clippy::too_many_arguments)]
pub async fn send_message_with_id(
    message_id: Option<String>,
    conversation_id: String,
    sender_id: String,
    content: String,
    attachments: Vec<OutgoingAttachment>,
    reply_to_id: Option<String>,
    thread_id: Option<String>,
    sender_username: Option<String>,
    state: &Arc<AppState>,
) -> Result<Message> {
    let id = match message_id {
        Some(id) => super::outbox::checked_message_id(id)?,
        None => Ulid::new().to_string(),
    };
    let queued = Queued {
        id,
        conversation_id,
        sender_id,
        content,
        reply_to_id,
        thread_id,
        sent_at: super::envelope_sent_at(),
    };
    if let super::outbox::Enqueued::AlreadySent(message) =
        super::outbox::enqueue(state, &queued, attachments).await?
    {
        return Ok(message);
    }
    // Test harness: stand in for the process dying right after the outbox row
    // was written (#1335).
    if test_hooks::take(test_hooks::Crash::AfterEnqueue) {
        return Err(test_hooks::simulated_crash());
    }
    super::outbox::deliver(state, &queued.id, sender_username).await
}

/// Deliver one queued message, under its outbox lock (`outbox::deliver`).
/// Clears `send_state` once the DS has the envelope; any error leaves it set
/// for the caller to mark failed.
pub(super) async fn deliver_queued(
    queued: &Queued,
    sender_username: Option<String>,
    state: &Arc<AppState>,
) -> Result<Message> {
    let Queued {
        id,
        conversation_id,
        sender_id,
        content,
        reply_to_id,
        thread_id,
        sent_at,
    } = queued.clone();
    state.check_not_outdated()?;

    // One request resolves the MLS group, checks membership and lists the DM
    // peers for the block gate (#987).
    let resolved =
        crate::commands::ds_reads::catch_up(state, &conversation_id, false, true).await?;
    // Not a member: recorded as `rejected`, not retried (#1335).
    if !resolved.authorized {
        return Err(super::outbox::not_a_member());
    }
    let is_channel = resolved.kind == pollis_api::directory::ConversationKind::Channel;
    let mls_group_id = resolved.mls_group_id.clone();

    // DM block enforcement: if either side blocks the other, the send appears
    // to succeed (stored locally) but is never encrypted, posted or broadcast,
    // so nothing reveals the block. Blocks in group channels are render-side
    // only.
    let suppress_delivery = if !is_channel {
        let peers: Vec<String> = resolved
            .dm
            .as_ref()
            .map(|dm| {
                dm.members
                    .iter()
                    .map(|m| m.user_id.clone())
                    .filter(|id| id != &sender_id)
                    .collect()
            })
            .unwrap_or_default();
        crate::commands::blocks::any_blocked_either_way(state, &sender_id, &peers).await?
    } else {
        false
    };

    if suppress_delivery {
        // The outbox row stays as the local-only copy; settling it is the whole
        // "delivery". Its empty ciphertext is never read.
        let sent_at = super::outbox::settle(state, &id).await?;
        return Ok(Message {
            id,
            conversation_id,
            sender_id,
            content: Some(content),
            reply_to_id,
            thread_id,
            sent_at,
        });
    }

    {
        let device_id = state.device_id.lock().await.clone();
        if let Some(ref did) = device_id {
            if let Err(e) = crate::commands::mls::poll_mls_welcomes_inner(state, &sender_id, did).await {
                crate::diag!("[messages] send_message: poll_mls_welcomes for {mls_group_id}: {e}");
            }
        }
    }

    // The interleaved catch-up, not the commit-only replay: advancing to head
    // would otherwise strand an unfetched current-epoch message (#440). No MLS
    // lock is held here, so it cannot deadlock.
    if let Err(e) = super::catch_up_mls_group_interleaved(state, &mls_group_id, &sender_id).await {
        crate::diag!("[messages] send_message: catch_up_mls_group for {mls_group_id}: {e}");
    }

    // Size padding (#331) for text; attachment envelopes stay unpadded (the R2
    // blob size already leaks it). A threaded send (#825) always takes the
    // padded frame, since its trailer has nowhere else to live.
    let plaintext: Vec<u8> = match thread_id.as_deref() {
        Some(thread) => super::framing::pad_threaded(content.as_bytes(), thread),
        None if super::edit_delete::is_attachment_content(&content) => {
            content.as_bytes().to_vec()
        }
        None => super::framing::pad(content.as_bytes()),
    };

    let (sealed, sent_at) = {
        // #1079: `seal::seal_under_lock`. MLS lock before `local_db`, dropped
        // before the post's own catch-up can want it.
        let mls_guard = state.mls_group_lock(&mls_group_id).await;
        let held = crate::state::MlsLockHeld::from_guard(&mls_guard);
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

        let sealed = super::seal::seal_under_lock(
            db.conn(),
            &mls_group_id,
            &conversation_id,
            &plaintext,
            &held,
        )?;

        // `sent_at` is refreshed on every attempt until the first seal (a
        // message queued offline lands when it went out), then fixed, since
        // the DS may already hold that stamp (#1335).
        let never_sealed: bool = db.conn().query_row(
            "SELECT length(ciphertext) = 0 FROM message WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )?;
        let sent_at = if never_sealed { super::envelope_sent_at() } else { sent_at };
        db.conn().execute(
            "UPDATE message SET ciphertext = ?1, sent_at = ?2 WHERE id = ?3",
            rusqlite::params![sealed.bytes, sent_at, id],
        )?;

        (sealed, sent_at)
    };

    // Sealed sender is unconditional (#607): no config can emit an unsealed
    // envelope (`flows/sealed_sender.rs`), which is why edit/delete authorship
    // is enforced client-side. It protects the at-rest envelope only; the DS
    // still sees the authenticated writer live (§2.1). The local row keeps the
    // real `sender_id`: it is the author's own copy.

    // Mentions (#843) are decided before the post, because the push audience
    // rides on it (#987). They resolve against this channel's roster only, so a
    // mention can never address or probe someone outside the room.
    let members: Vec<(String, String)> = if is_channel {
        crate::commands::ds_reads::group(state, &mls_group_id, &sender_id, false)
            .await?
            .members
            .into_iter()
            .filter(|m| m.user_id != sender_id)
            // A member without a `users` row still counts for `@all`.
            .map(|m| (m.user_id, m.username.unwrap_or_default()))
            .collect()
    } else {
        Vec::new()
    };
    let audience = if is_channel {
        mention_audience(is_channel, &content, &members)
    } else {
        MentionAudience::Everyone
    };

    // Content-free push (#344), fanned out by the DS. Since #1431 every message
    // pushes every other member; the DS skips members who muted the channel
    // unless `push_mentions` names them directly. A DM has no mention list.
    let direct: Vec<String> = if is_channel {
        direct_mentions(&content, &members)
    } else {
        Vec::new()
    };
    let push_to: Option<Vec<String>> = Some(Vec::new());
    let push_mentions: Option<Vec<String>> = if is_channel {
        Some(direct.clone())
    } else {
        None
    };

    // The deletion capability (#1086) binds to the envelope id, which a re-seal
    // does not change, so one hash serves every attempt.
    let delete_token_hash =
        super::delete_capability::envelope_delete_token_hash(state, &id).await;

    // If a commit landed since the catch-up, the DS refuses the epoch (#1041)
    // and `post_resealing` re-seals and posts again.
    super::seal::post_resealing(
        state,
        super::seal::Target {
            mls_group_id: &mls_group_id,
            conversation_id: &conversation_id,
            user_id: &sender_id,
            plaintext: &plaintext,
            envelope_id: &id,
        },
        sealed,
        |sealed| pollis_api::messages::SendMessageBody {
            id: id.clone(),
            conversation_id: conversation_id.clone(),
            sender_id: Some(SEALED_SENDER_SENTINEL.to_string()),
            ciphertext: test_hooks::maybe_corrupt(sealed.wire()),
            reply_to_id: reply_to_id.clone(),
            // The send's own stamp, identical on every attempt (#1335).
            sent_at: sent_at.clone(),
            sealed: 1,
            generation: Some(sealed.generation),
            epoch: Some(sealed.epoch),
            push_to: push_to.clone(),
            push_mentions: push_mentions.clone(),
            delete_token_hash: delete_token_hash.clone(),
        },
        |conn, sealed| {
            conn.execute(
                "UPDATE message SET ciphertext = ?1 WHERE id = ?2",
                rusqlite::params![sealed.bytes, id],
            )?;
            Ok(())
        },
    )
    .await?;

    // Test harness: the process dies after the DS took the envelope but before
    // this device recorded it (#1335).
    if test_hooks::take(test_hooks::Crash::AfterPost) {
        return Err(test_hooks::simulated_crash());
    }
    // Everything below is best-effort signalling that never fails a send.
    super::outbox::settle(state, &id).await?;
    let now = sent_at;

    // Server-side attachment references (#690). Best effort.
    if super::edit_delete::is_attachment_content(&content) {
        let _ = super::edit_delete::register_attachment_refs(state, &id, &content).await;
    }

    // Non-fatal. The wake-up carries routing only, no sender (§5).
    if is_channel {
        // One LiveKit room per group; receivers filter by channel_id.
        if let Err(e) = crate::commands::livekit::publish_new_message_to_room(
            state,
            &mls_group_id,
            Some(&conversation_id),
            None,
        ).await {
            crate::diag!("[realtime] send_message: publish to group {mls_group_id}: {e}");
        }
    } else {
        // DM: the room is the conversation id.
        if let Err(e) = crate::commands::livekit::publish_new_message_to_room(
            state,
            &conversation_id,
            None,
            Some(&conversation_id),
        ).await {
            crate::diag!("[realtime] send_message: publish to DM room {conversation_id}: {e}");
        }
    }

    // Inbox pings. Fire-and-forget.
    if is_channel {
        // A member also named directly gets `user_mention` instead (#1431):
        // that is the ping that gets through a mute.
        if mentions_all(&content) {
            let payload = serde_json::json!({
                "type": "all_mention",
                "group_id": mls_group_id,
                "channel_id": conversation_id,
                "sender_id": sender_id,
                "sender_username": sender_username,
            });
            let named = serde_json::json!({
                "type": "user_mention",
                "group_id": mls_group_id,
                "channel_id": conversation_id,
                "sender_id": sender_id,
                "sender_username": sender_username,
            });
            for (uid, _) in &members {
                let ping = if direct.contains(uid) { &named } else { &payload };
                if let Err(e) =
                    crate::commands::livekit::publish_to_user_inbox(state, uid, ping.clone())
                        .await
                {
                    crate::diag!("[realtime] send_message: @all inbox publish to {uid}: {e}");
                }
            }
        } else if let MentionAudience::Only(ref ids) = audience {
            let payload = serde_json::json!({
                "type": "user_mention",
                "group_id": mls_group_id,
                "channel_id": conversation_id,
                "sender_id": sender_id,
                "sender_username": sender_username,
            });
            for uid in ids {
                if let Err(e) =
                    crate::commands::livekit::publish_to_user_inbox(state, uid, payload.clone())
                        .await
                {
                    crate::diag!("[realtime] send_message: @mention inbox publish to {uid}: {e}");
                }
            }
        }
    }

    Ok(Message {
        id,
        conversation_id,
        sender_id,
        content: Some(content),
        reply_to_id,
        thread_id,
        sent_at: now,
    })
}

/// True when `content` contains `@all` as a standalone, case-insensitive token
/// ("@all," matches; "@allison" and "email@allcorp" do not).
fn mentions_all(content: &str) -> bool {
    content.split_whitespace().any(|w| {
        w.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '@')
            .eq_ignore_ascii_case("@all")
    })
}

/// Characters allowed inside a username after the `@`: wide, since
/// `users.username` has no charset constraint.
fn is_mention_body_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '.'
}

/// The `@` mention tokens in `content`, lowercased, without the `@`. An `@`
/// counts only at the start or after whitespace; trailing `.`/`-`/`_` are
/// trimmed; `@all` is excluded.
///
/// MIRROR: `mentionTokens()` in `frontend/src/utils/mentions.ts`. The two must
/// agree or the composer will promise a ping the backend does not send.
fn mention_tokens(content: &str) -> Vec<String> {
    let chars: Vec<char> = content.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '@' {
            i += 1;
            continue;
        }
        if i > 0 && !chars[i - 1].is_whitespace() {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && is_mention_body_char(chars[j]) {
            j += 1;
        }
        let token: String = chars[i + 1..j]
            .iter()
            .collect::<String>()
            .trim_end_matches(['.', '-', '_'])
            .to_lowercase();
        if !token.is_empty() && token != "all" {
            tokens.push(token);
        }
        i = j.max(i + 1);
    }
    tokens
}

/// Who gets an inbox ping and a background push, beyond the unread badge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MentionAudience {
    /// Everyone but the sender: a DM, or a channel `@all`.
    Everyone,
    /// Only the members named by `@username`.
    Only(Vec<String>),
    /// Ordinary channel chatter.
    Nobody,
}

/// Decide the wake-up audience for a message. `members` is `(user_id,
/// username)` for every other member of this conversation, so an unknown
/// `@name` resolves to nothing and reveals nothing. Case-insensitive; ids in
/// `members` order, deduplicated.
pub(crate) fn mention_audience(
    is_channel: bool,
    content: &str,
    members: &[(String, String)],
) -> MentionAudience {
    if !is_channel {
        return MentionAudience::Everyone;
    }
    if mentions_all(content) {
        return MentionAudience::Everyone;
    }
    let ids = direct_mentions(content, members);
    if ids.is_empty() {
        MentionAudience::Nobody
    } else {
        MentionAudience::Only(ids)
    }
}

/// The members `content` names directly by `@username`, `@all` or not: the
/// ones a channel mute does not silence (#1431). Matching as
/// [`mention_audience`].
pub(crate) fn direct_mentions(content: &str, members: &[(String, String)]) -> Vec<String> {
    let tokens = mention_tokens(content);
    let mut ids: Vec<String> = Vec::new();
    if tokens.is_empty() {
        return ids;
    }
    for (user_id, username) in members {
        let uname = username.to_lowercase();
        if !uname.is_empty() && tokens.contains(&uname) && !ids.contains(user_id) {
            ids.push(user_id.clone());
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members() -> Vec<(String, String)> {
        vec![
            ("u_dana".to_string(), "dana".to_string()),
            ("u_bob".to_string(), "Bob".to_string()),
            ("u_ann".to_string(), "ann_marie".to_string()),
        ]
    }

    // #843: naming someone in a channel wakes them.
    #[test]
    fn per_user_mention_in_channel_targets_only_the_named_member() {
        assert_eq!(
            mention_audience(true, "can you look at this @dana", &members()),
            MentionAudience::Only(vec!["u_dana".to_string()])
        );
    }

    // A plain channel message wakes nobody.
    #[test]
    fn plain_channel_message_wakes_nobody() {
        assert_eq!(
            mention_audience(true, "shipping the build now", &members()),
            MentionAudience::Nobody
        );
    }

    #[test]
    fn mention_matching_is_case_insensitive_and_dedupes() {
        assert_eq!(
            mention_audience(true, "@BOB @bob ping", &members()),
            MentionAudience::Only(vec!["u_bob".to_string()])
        );
    }

    #[test]
    fn multiple_mentions_preserve_member_order() {
        assert_eq!(
            mention_audience(true, "@bob and @dana", &members()),
            MentionAudience::Only(vec!["u_dana".to_string(), "u_bob".to_string()])
        );
    }

    // Trailing sentence punctuation must not swallow the mention.
    #[test]
    fn trailing_punctuation_is_trimmed() {
        assert_eq!(
            mention_audience(true, "thanks @dana.", &members()),
            MentionAudience::Only(vec!["u_dana".to_string()])
        );
        assert_eq!(
            mention_audience(true, "hey @ann_marie, look", &members()),
            MentionAudience::Only(vec!["u_ann".to_string()])
        );
    }

    // No-leak: mentions never reach outside the room.
    #[test]
    fn unknown_and_embedded_mentions_resolve_to_nobody() {
        assert_eq!(
            mention_audience(true, "@carol are you there", &members()),
            MentionAudience::Nobody
        );
        assert_eq!(
            mention_audience(true, "mail me at bob@dana.com", &members()),
            MentionAudience::Nobody
        );
    }

    // `@all` keeps its existing everyone-fanout, and never reads as a username.
    #[test]
    fn all_mention_still_means_everyone() {
        assert_eq!(
            mention_audience(true, "@all standup in 5", &members()),
            MentionAudience::Everyone
        );
        assert_eq!(mention_tokens("@all standup"), Vec::<String>::new());
    }

    // A DM notifies its recipient whether or not anyone is named.
    #[test]
    fn dm_always_notifies_everyone() {
        assert_eq!(
            mention_audience(false, "hey", &members()),
            MentionAudience::Everyone
        );
        assert_eq!(
            mention_audience(false, "hey @dana", &members()),
            MentionAudience::Everyone
        );
    }

    #[test]
    fn mention_tokens_ignores_bare_at_and_embedded_at() {
        assert_eq!(mention_tokens("@ hello"), Vec::<String>::new());
        assert_eq!(mention_tokens("user@example.com"), Vec::<String>::new());
        assert_eq!(mention_tokens("hi @dana"), vec!["dana".to_string()]);
    }

    // #1431: a name inside an `@all` still counts; `@all` itself names nobody.
    #[test]
    fn direct_mentions_survive_an_all_mention() {
        assert_eq!(
            direct_mentions("@all and especially @Dana", &members()),
            vec!["u_dana".to_string()]
        );
        assert_eq!(direct_mentions("@all standup", &members()), Vec::<String>::new());
        assert_eq!(direct_mentions("plain chatter", &members()), Vec::<String>::new());
        assert_eq!(direct_mentions("@carol not here", &members()), Vec::<String>::new());
    }
}

/// Test-only hooks (#1312, #1335): simulated crashes, stubbed uploads, and a
/// corrupted next send. Without `test-harness` they are inert.
pub mod test_hooks {
    /// Where a simulated process death cuts a send short (#1335).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Crash {
        /// The outbox row is written; nothing has touched the network.
        AfterEnqueue,
        /// The DS holds the envelope; this device has not recorded that.
        AfterPost,
    }

    #[cfg(feature = "test-harness")]
    static NEXT_CRASH: std::sync::Mutex<Option<Crash>> = std::sync::Mutex::new(None);

    /// Make the next send in this process "die" at `at`. One-shot.
    #[cfg(feature = "test-harness")]
    pub fn crash_next_send(at: Crash) {
        *NEXT_CRASH.lock().expect("NEXT_CRASH poisoned") = Some(at);
    }

    /// Consume an armed crash at `at`. Always `false` outside the harness.
    pub(super) fn take(at: Crash) -> bool {
        #[cfg(feature = "test-harness")]
        {
            let mut armed = NEXT_CRASH.lock().expect("NEXT_CRASH poisoned");
            if *armed == Some(at) {
                *armed = None;
                return true;
            }
        }
        let _ = at;
        false
    }

    #[cfg(feature = "test-harness")]
    static STUB_UPLOADS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Answer outbox attachment uploads without R2, which the flows world does
    /// not have (#1335). Stays set until cleared.
    #[cfg(feature = "test-harness")]
    pub fn stub_attachment_uploads(on: bool) {
        STUB_UPLOADS.store(on, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(feature = "test-harness")]
    pub(crate) fn uploads_stubbed() -> bool {
        STUB_UPLOADS.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(feature = "test-harness")]
    static ATTEMPTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// Delivery attempts this process has run (#1335) — how a test tells one
    /// drain from two.
    #[cfg(feature = "test-harness")]
    pub fn delivery_attempts() -> usize {
        ATTEMPTS.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(feature = "test-harness")]
    pub(crate) fn count_attempt() {
        ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    const SIMULATED_CRASH: &str = "simulated process death (test harness)";

    pub(super) fn simulated_crash() -> crate::error::Error {
        crate::error::Error::Other(anyhow::anyhow!(SIMULATED_CRASH))
    }

    /// A dead process marks nothing failed, so neither may its stand-in.
    pub(crate) fn is_simulated_crash(e: &crate::error::Error) -> bool {
        e.to_string().contains(SIMULATED_CRASH)
    }

    #[cfg(feature = "test-harness")]
    static CORRUPT_NEXT_SEND: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// Corrupt the ciphertext of the next message this process sends. One-shot.
    #[cfg(feature = "test-harness")]
    pub fn corrupt_next_send() {
        CORRUPT_NEXT_SEND.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Flip the low bit of the envelope's last byte, inside the AEAD tag: the
    /// MLS header still parses and the DS accepts it, but nobody can open it.
    pub(super) fn maybe_corrupt(wire: String) -> String {
        #[cfg(feature = "test-harness")]
        {
            if CORRUPT_NEXT_SEND.swap(false, std::sync::atomic::Ordering::SeqCst) {
                let split = wire.len() - 2;
                if let Ok(last) = u8::from_str_radix(&wire[split..], 16) {
                    return format!("{}{:02x}", &wire[..split], last ^ 0x01);
                }
            }
        }
        wire
    }
}
