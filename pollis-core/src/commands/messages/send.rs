use std::sync::Arc;
use ulid::Ulid;

use crate::error::Result;
use crate::state::AppState;

use super::outbox::{OutgoingAttachment, Queued};
use super::types::Message;

/// Placeholder for the NOT NULL `message_envelope.sender_id` under sealed sender (#331).
pub const SEALED_SENDER_SENTINEL: &str = "sealed";

/// Send a message under a fresh id. See [`send_message_with_id`].
pub async fn send_message(
    conversation_id: String,
    sender_id: String,
    content: String,
    reply_to_id: Option<String>,
    // Thread root ULID (#825); travels only inside the MLS ciphertext.
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
/// Written locally as `pending` before any network work; only a confirmed post clears it.
///
/// `message_id` makes a repeat call idempotent.
///
/// `attachments` are stored with the row and uploaded by delivery; `content` is then the caption.
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
    // Test-harness only: die right after the outbox row is written (#1335).
    if test_hooks::take(test_hooks::Crash::AfterEnqueue) {
        return Err(test_hooks::simulated_crash());
    }
    super::outbox::deliver(state, &queued.id, sender_username).await
}

/// Deliver one queued message (the outbox already holds the local row).
///
/// Runs under the message's outbox lock (`outbox::deliver`). Clears
/// `send_state` once the DS has the envelope; any error leaves it set, and the
/// caller marks it failed.
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

    // Group channels share the group's MLS group; a DM's is keyed by conversation_id.
    let resolved =
        crate::commands::ds_reads::catch_up(state, &conversation_id, false, true).await?;
    // Not (or no longer) a member: no attempt will ever get further, so say so
    // in a way the outbox records as `rejected` rather than retrying (#1335).
    if !resolved.authorized {
        return Err(super::outbox::not_a_member());
    }
    let is_channel = resolved.kind == pollis_api::directory::ConversationKind::Channel;
    let mls_group_id = resolved.mls_group_id.clone();

    // DM blocks silently drop the message: stored locally, never sent, so no signal reveals the block.
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
        // The outbox row stays as the local-only copy; settling it is the whole "delivery".
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

    // Poll MLS Welcomes — this device may have been added to the group but
    // hasn't applied the Welcome yet.
    {
        let device_id = state.device_id.lock().await.clone();
        if let Some(ref did) = device_id {
            if let Err(e) = crate::commands::mls::poll_mls_welcomes_inner(state, &sender_id, did).await {
                crate::diag!("[messages] send_message: poll_mls_welcomes for {mls_group_id}: {e}");
            }
        }
    }

    // Ingest-before-advance (#440): interleaved catch-up so no inbound keys are discarded; no lock held here.
    if let Err(e) = super::catch_up_mls_group_interleaved(state, &mls_group_id, &sender_id).await {
        crate::diag!("[messages] send_message: catch_up_mls_group for {mls_group_id}: {e}");
    }

    // #1571: re-add a leafless device before sealing so it can read this. Best-effort.
    // Boxed: a reconcile is a large future, and inlining it into every send's
    // state machine overflowed a default (2 MiB) test-thread stack.
    Box::pin(crate::commands::mls::backstop_before_seal(state, &mls_group_id, &sender_id)).await;

    // Pad text to a size bucket (#331); attachments stay unpadded unless threaded (#825), whose id rides the frame.
    let plaintext: Vec<u8> = match thread_id.as_deref() {
        Some(thread) => super::framing::pad_threaded(content.as_bytes(), thread),
        None if super::edit_delete::is_attachment_content(&content) => {
            content.as_bytes().to_vec()
        }
        None => super::framing::pad(content.as_bytes()),
    };

    let (sealed, sent_at) = {
        // #1079: serialize behind any committer on this device before sealing —
        // see `seal::seal_under_lock`. Taken before `local_db` (reconcile's and
        // self_update's order) and dropped with this block, before the post's
        // own catch-up can want it.
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

        // `sent_at` refreshes per attempt until the first seal, then stays fixed (the DS may hold it).
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

    // Mentions (#843) resolve against this channel's roster only, before the post carries the push audience.
    let members: Vec<(String, String)> = if is_channel {
        crate::commands::ds_reads::group(state, &mls_group_id, &sender_id, false)
            .await?
            .members
            .into_iter()
            .filter(|m| m.user_id != sender_id)
            // A member with no `users` row still counts for `@all`.
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

    // Push fan-out is on the DS (#987, #1431); `direct` names who gets through a mute.
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

    // Post via the DS; on an epoch-behind refusal (#1041) `post_resealing` re-seals and retries.
    // The deletion capability (#1086), computed once: re-seals keep the envelope id.
    let delete_token_hash =
        super::delete_capability::envelope_delete_token_hash(state, &id).await;

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
            // The send's own stamp, not the seal's (#1335): identical on every
            // attempt, so a replay and its retry agree.
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

    // Test-harness only: die after the DS took the envelope (#1335).
    if test_hooks::take(test_hooks::Crash::AfterPost) {
        return Err(test_hooks::simulated_crash());
    }
    // Delivered: the DS holds the envelope, so the outbox entry is done.
    // Everything below is best-effort signalling that never fails a send.
    super::outbox::settle(state, &id).await?;
    let now = sent_at;

    // Register attachment references (#690) so the DS refcounts objects. Best-effort.
    if super::edit_delete::is_attachment_content(&content) {
        let _ = super::edit_delete::register_attachment_refs(state, &id, &content).await;
    }

    // Notify via LiveKit (non-fatal); the wake-up carries routing only, no sender.
    if is_channel {
        // One room per group; receivers filter by channel_id.
        if let Err(e) = crate::commands::livekit::publish_new_message_to_room(
            state,
            &mls_group_id,
            Some(&conversation_id),
            None,
        ).await {
            crate::diag!("[realtime] send_message: publish to group {mls_group_id}: {e}");
        }
    } else {
        // DM: publish to the shared DM room.
        if let Err(e) = crate::commands::livekit::publish_new_message_to_room(
            state,
            &conversation_id,
            None,
            Some(&conversation_id),
        ).await {
            crate::diag!("[realtime] send_message: publish to DM room {conversation_id}: {e}");
        }
    }

    // Inbox pings; fire-and-forget.
    if is_channel {
        // A member also named directly gets `user_mention`, which gets through a mute (#1431).
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

/// True when `content` contains an `@all` mention as a standalone token —
/// i.e. whitespace-delimited and ignoring trailing punctuation, so "@all" and
/// "@all," match but "@allison" and "email@allcorp" do not. Case-insensitive.
fn mentions_all(content: &str) -> bool {
    content.split_whitespace().any(|w| {
        w.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '@')
            .eq_ignore_ascii_case("@all")
    })
}

/// Characters that may appear INSIDE a username after the leading `@`. Kept
/// deliberately wide (the `users.username` column has no charset constraint)
/// but closed over the punctuation that normally ends a sentence, so
/// "@dana." and "@dana," yield the token `dana`.
fn is_mention_body_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '.'
}

/// Extract the `@`-prefixed mention tokens from `content`, lowercased and
/// stripped of the leading `@`.
///
/// Only a string-initial or whitespace-preceded `@` counts; `@all` is excluded.
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
        // Only a string-initial or whitespace-preceded `@` opens a mention.
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

/// Who should be woken for a message — i.e. who gets an inbox ping and a
/// background push, over and above the unread badge every recipient gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MentionAudience {
    /// Everyone in the conversation except the sender. A DM (always personal)
    /// or a channel `@all`.
    Everyone,
    /// Only these user ids — a channel message naming members by `@username`.
    Only(Vec<String>),
    /// Nobody — ordinary channel chatter. The accompanying `new_message`
    /// event still bumps the unread badge.
    Nobody,
}

/// Decide the wake-up audience for a message.
///
/// `members` is every other member of the conversation, so an `@name` never reaches outside the room.
pub(crate) fn mention_audience(
    is_channel: bool,
    content: &str,
    members: &[(String, String)],
) -> MentionAudience {
    // A DM is personal by construction — every message notifies.
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

/// The members `content` names directly by `@username`, whether or not it also
/// says `@all` — the ones a channel mute does not silence (#1431).
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

    // The invariant #843 exists to establish: naming someone in a CHANNEL
    // wakes them, even though the same channel's ordinary chatter does not.
    #[test]
    fn per_user_mention_in_channel_targets_only_the_named_member() {
        assert_eq!(
            mention_audience(true, "can you look at this @dana", &members()),
            MentionAudience::Only(vec!["u_dana".to_string()])
        );
    }

    // The other half of the same invariant: a plain channel message wakes
    // nobody. Regressing this turns every channel into a notification storm.
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

    // An `@name` nobody in this conversation answers to resolves to nothing.
    // This is the no-leak property: mentions never reach outside the room.
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

    // #1431: the list that gets through a mute. A name inside an `@all` still
    // counts — being named is what matters — and `@all` itself names nobody.
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

/// Test-only hooks (crashes, corruption); compiled out without `test-harness`.
pub mod test_hooks {
    /// Where a simulated process death cuts a send short (#1335).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Crash {
        /// The outbox row is written; nothing has touched the network.
        AfterEnqueue,
        /// The DS holds the envelope; this device has not recorded that.
        AfterPost,
        /// The envelope is sealed (the sender's ratchet has moved on) and
        /// nothing has been posted (#1524).
        AfterSeal,
        /// A receiving device opened an envelope (#1524) but has not written the row; unwinds.
        AfterDecrypt,
        /// An edit is stored in the edit outbox; nothing has touched the network (#1555).
        EditAfterEnqueue,
        /// The DS holds the edit envelope; this device has not recorded that
        /// (#1555).
        EditAfterPost,
    }

    #[cfg(feature = "test-harness")]
    static NEXT_CRASH: std::sync::Mutex<Option<Crash>> = std::sync::Mutex::new(None);

    /// Make the next send in this process "die" at `at`. One-shot.
    #[cfg(feature = "test-harness")]
    pub fn crash_next_send(at: Crash) {
        *NEXT_CRASH.lock().expect("NEXT_CRASH poisoned") = Some(at);
    }

    /// Disarm any crash armed and never reached, so it cannot cut short an
    /// operation it was not meant for.
    #[cfg(feature = "test-harness")]
    pub fn disarm_crash() {
        *NEXT_CRASH.lock().expect("NEXT_CRASH poisoned") = None;
    }

    /// Consume an armed crash at `at`. Always `false` outside the harness.
    pub(crate) fn take(at: Crash) -> bool {
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

    pub(crate) fn simulated_crash() -> crate::error::Error {
        crate::error::Error::Other(anyhow::anyhow!(SIMULATED_CRASH))
    }

    /// The stand-in for a death where there is no error to return: unwind, so
    /// nothing after this point runs and every open savepoint rolls back.
    pub(crate) fn die() -> ! {
        panic!("{SIMULATED_CRASH}")
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

    /// Flip the low bit of the envelope's last byte — inside the AEAD tag, so the
    /// MLS header (group, epoch) still parses and the DS still accepts it, but no
    /// recipient can open it.
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
