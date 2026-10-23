use std::sync::Arc;
use ulid::Ulid;

use crate::error::Result;
use crate::state::AppState;

use super::outbox::{OutgoingAttachment, Queued};
use super::types::Message;

/// Non-identifying placeholder written into the still-NOT-NULL
/// `message_envelope.sender_id` column when sealed sender is enabled (issue
/// #331). A fixed sentinel (rather than a per-message random token) carries zero
/// joinable information and is smaller than a real ULID; the true sender lives in
/// the MLS credential inside the ciphertext. See
/// `docs/metadata-minimization-design.md` §2.1.
pub const SEALED_SENDER_SENTINEL: &str = "sealed";

/// Send a message under a fresh id. See [`send_message_with_id`].
pub async fn send_message(
    conversation_id: String,
    sender_id: String,
    content: String,
    reply_to_id: Option<String>,
    // ULID of the thread root this message replies into (#825). `None` for an
    // ordinary channel message. Travels only inside the MLS ciphertext — it is
    // never included in the DS request body.
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
/// The message is written to the local `message` table as `send_state =
/// 'pending'` BEFORE any network work — that row is the outbox entry. Only then
/// does delivery (catch-up, seal, DS post) run, and only a confirmed post clears
/// the state. If this process dies part-way, the row survives and the outbox
/// drain delivers it after the next unlock or reconnect; if the attempt fails,
/// the row is marked `'failed'` and the renderer offers Retry. Either way a
/// message the user saw accepted is never silently gone.
///
/// `message_id` lets the renderer name the message before it calls (from
/// `new_message_id`), so its optimistic row and the stored row are the same
/// message. A second call with the same id is idempotent: an unsent row is
/// delivered (never duplicated), a delivered one is returned as it stands.
///
/// `attachments` makes the outbox own the upload too: their bytes are read and
/// stored with the row before anything touches the network, `content` is then
/// the caption, and delivery uploads them and builds the `_att` envelope. A
/// caller that uploaded already (an older renderer, the TUI) passes none and
/// the finished envelope as `content`, exactly as before.
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
    // Test-harness only: stand in for the process dying right after the
    // outbox row was written (#1335) — nothing marks it failed, exactly as
    // with a real kill.
    if test_hooks::take(test_hooks::Crash::AfterEnqueue) {
        return Err(test_hooks::simulated_crash());
    }
    super::outbox::deliver(state, &queued.id, sender_username).await
}

/// Deliver one queued message: everything `send_message` did before #1335,
/// minus writing the local row, which the outbox already holds.
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

    // For group channels, all channels share the group's MLS group (keyed by group_id).
    // For DM conversations, the MLS group is keyed by conversation_id directly.
    // is_channel = true means conversation_id is a channel ID; group_id is the LiveKit room name.
    //
    // ONE request answers the resolve, the membership check and the DM peer list
    // (which the block gate below consumes) — three dependent round trips before
    // #987, and the peer list was itself a query per peer.
    let resolved =
        crate::commands::ds_reads::catch_up(state, &conversation_id, false, true).await?;
    // Not (or no longer) a member: no attempt will ever get further, so say so
    // in a way the outbox records as `rejected` rather than retrying (#1335).
    if !resolved.authorized {
        return Err(super::outbox::not_a_member());
    }
    let is_channel = resolved.kind == pollis_api::directory::ConversationKind::Channel;
    let mls_group_id = resolved.mls_group_id.clone();

    // Block enforcement for DMs: if any other participant in this DM
    // has a block relationship with the sender (either direction),
    // silently drop the message. The send appears to succeed to the
    // sender (message is stored locally so their own history looks
    // consistent) but it is NOT encrypted, NOT posted to Turso, and
    // NOT broadcast on LiveKit — the recipient never sees it and no
    // observable signal reveals the block. Group channels are not
    // gated here; blocks in groups are purely render-side on the
    // blocker's client.
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
        // The outbox row stays as the local-only copy, so the sender's own
        // conversation view stays consistent (history survives reloads). Its
        // empty ciphertext is fine — nothing will ever decrypt this row; it's
        // only read back via the `content` column. Settling it is the whole
        // "delivery".
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

    // Catch this device up to head with the INTERLEAVED ingesting catch-up,
    // decrypting every bound conversation's messages at each epoch BEFORE the
    // shared local group advances past it. A bare commit-only replay
    // (`process_pending_commits_inner`) would reach head immediately, and with
    // `max_past_epochs = 0` a current-epoch inbound message we haven't fetched
    // yet would have its keys discarded the instant we advance past its epoch
    // (issue #440, the committer strand — a send that catches up commit-only
    // strands an un-ingested inbound message). The interleaved catch-up is a
    // superset: it still reaches head (creating/repairing the local group via
    // external-join if needed), just decrypting en route. Safe to call here —
    // send_message holds no MLS group lock, so re-acquiring it inside the
    // catch-up cannot deadlock.
    if let Err(e) = super::catch_up_mls_group_interleaved(state, &mls_group_id, &sender_id).await {
        crate::diag!("[messages] send_message: catch_up_mls_group for {mls_group_id}: {e}");
    }

    // Size padding (issue #331 v2, `docs/metadata-minimization-design.md`
    // §4.1). Pad TEXT plaintext to a size bucket before encryption so the
    // ciphertext length in `message_envelope` no longer reveals the message
    // length. The framing (version byte + length prefix) lives inside the
    // MLS ciphertext, so only members see it and there's no schema/server
    // change. Attachment envelopes are left unpadded — their R2 blob size is
    // inherent and dedup depends on it.
    //
    // A THREADED send (#825) always takes the padded frame, attachment
    // envelope or not, because the thread id rides in that frame's trailer
    // and there is nowhere else inside the ciphertext to put it. Padding an
    // attachment envelope is harmless — the exemption above exists because
    // the R2 blob size already leaks, not because padding would hurt — and
    // an old client still strips a `0xF5` frame correctly, so a threaded
    // attachment stays readable on clients that predate threads.
    //
    // An UNTHREADED send is byte-for-byte what it was before threads.
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

        // The outbox row (#1335) already holds everything but the ciphertext.
        // Its `sent_at`: refreshed by every attempt until the first seal (a
        // message queued offline lands at the time it went out, not when it
        // was typed), then fixed — this seal may post, and every later
        // attempt must post the same stamp the DS may already hold.
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

    // Sealed sender (issue #331, `docs/metadata-minimization-design.md` §2) —
    // UNCONDITIONAL (#607). We always blind the server-visible `message_envelope`:
    // `sealed = 1` and a fixed, non-identifying sentinel in the still-NOT-NULL
    // `sender_id` column, so a Turso breach / subpoena of the stored table no
    // longer reveals sender-per-message. Attribution is unaffected — recipients
    // take the true sender from the MLS credential inside the ciphertext (the
    // reader, already shipped), so the sentinel never has to decode. There is no
    // opt-out: no config flag can emit an UNSEALED envelope (invariant test in
    // `flows/sealed_sender.rs`), which is what lets edit/delete authorization move
    // fully client-side (Solution A) — the DS can no longer see who authored a
    // message, so it stops trying to enforce authorship.
    //
    // Scope (be honest, §2.1): this defends the AT-REST envelope only. The DS
    // still authenticates every write with an `X-Pollis-User` header and gates on
    // membership, so a *live* DS operator still sees the sender in real time.
    // Closing that axis is v1.5 (anonymous membership proof).
    //
    // The LOCAL `message` row (inserted above) deliberately keeps the real
    // `sender_id`: it is the author's own decrypted copy on their trusted device,
    // where the field is self-attribution and the at-rest server threat does not
    // apply. Sealing it would mislabel the author's own message, since the send
    // path writes attribution directly rather than re-deriving it from the
    // credential the way the ingest reader does.

    // Mentions (#843) — decided HERE, before the envelope post, because the push
    // audience now rides on that write (#987).
    //
    // Group messages don't raise OS notifications for every new message, but a
    // mention does: an explicit `@all` pings every group member's inbox, and an
    // `@username` pings exactly the members named. Both resolve against THIS
    // channel's roster, which the sender is already a member of, so a mention can
    // never address (or probe for) someone outside the room. Per-user
    // "notifications off" is enforced client-side in notify.ts. Only meaningful
    // for group channels; a DM already notifies its recipient unconditionally.
    let members: Vec<(String, String)> = if is_channel {
        crate::commands::ds_reads::group(state, &mls_group_id, &sender_id, false)
            .await?
            .members
            .into_iter()
            .filter(|m| m.user_id != sender_id)
            // A member whose `users` row is missing still counts for `@all` — it
            // just cannot be named individually, which is exactly what the LEFT
            // JOIN's empty-string default expressed.
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

    // Content-free push to recipients' backgrounded/closed apps (#344), fanned
    // out by the DS since #987 — the client no longer reads other people's
    // `push_token` rows, and the two round trips it took to do so are gone.
    //
    // Notification policy is driven by the SAME `audience` the inbox fanout
    // below uses, so the two cannot drift: a DM always notifies its recipient, a
    // channel `@all` notifies every member, a channel `@username` notifies
    // exactly the members named, and ordinary channel chatter notifies nobody
    // (it would be far too noisy — desktop raises no per-message notification
    // for it either).
    let push_to: Option<Vec<String>> = match &audience {
        MentionAudience::Everyone => Some(Vec::new()),
        MentionAudience::Only(ids) => Some(ids.clone()),
        MentionAudience::Nobody => None,
    };

    // Post to Turso for offline delivery. DS seam: route the envelope write
    // through the Delivery Service (the write API). The body asserts the epoch
    // the envelope was sealed at; if a commit landed since our catch-up the DS
    // refuses it (#1041) and `post_resealing` catches up, re-seals at the new
    // The per-envelope deletion capability (#1086). Computed once, outside the
    // re-seal closure: it binds to the envelope id, which a re-seal does not
    // change, so the same hash is correct for every attempt.
    let delete_token_hash =
        super::delete_capability::envelope_delete_token_hash(state, &id).await;

    // epoch, rewrites our own local copy to match, and posts again. What landed
    // is not needed past here: the row keeps the send's own `sent_at` (#1335).
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

    // Test-harness only: the process dies after the DS took the envelope but
    // before this device recorded it (#1335) — the retry must not deliver it
    // twice.
    if test_hooks::take(test_hooks::Crash::AfterPost) {
        return Err(test_hooks::simulated_crash());
    }
    // Delivered: the DS holds the envelope, so the outbox entry is done.
    // Everything below is best-effort signalling that never fails a send.
    super::outbox::settle(state, &id).await?;
    let now = sent_at;

    // Register a server-side reference for each attachment this message carries
    // (#690), keyed by the message id. The DS reference-counts the
    // `attachment_object` row so deleting one message that names an object
    // cannot strand another message that still does. Best-effort — never fails the
    // send (the envelope above is the durable delivery).
    if super::edit_delete::is_attachment_content(&content) {
        let _ = super::edit_delete::register_attachment_refs(state, &id, &content).await;
    }

    // Notify recipients via LiveKit. Non-fatal — errors are logged, not returned.
    // §5 signalling minimization: the wake-up carries conversation routing only,
    // no sender — recipients attribute the message from the decrypted envelope.
    if is_channel {
        // One LiveKit room per group covers all its channels.
        // Receivers filter by channel_id in the event payload.
        if let Err(e) = crate::commands::livekit::publish_new_message_to_room(
            state,
            &mls_group_id,
            Some(&conversation_id),
            None,
        ).await {
            crate::diag!("[realtime] send_message: publish to group {mls_group_id}: {e}");
        }
    } else {
        // DM: publish directly to the shared DM room (conversation_id is the room name).
        // Both participants are connected to this room via connect_rooms.
        if let Err(e) = crate::commands::livekit::publish_new_message_to_room(
            state,
            &conversation_id,
            None,
            Some(&conversation_id),
        ).await {
            crate::diag!("[realtime] send_message: publish to DM room {conversation_id}: {e}");
        }
    }

    // Inbox pings for the audience decided above. Fire-and-forget; failures are
    // logged, never fatal to the send.
    if is_channel {
        // `@all` keeps its exact existing payload and fanout — every member.
        if mentions_all(&content) {
            let payload = serde_json::json!({
                "type": "all_mention",
                "group_id": mls_group_id,
                "channel_id": conversation_id,
                "sender_id": sender_id,
                "sender_username": sender_username,
            });
            for (uid, _) in &members {
                if let Err(e) =
                    crate::commands::livekit::publish_to_user_inbox(state, uid, payload.clone())
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
/// A token only counts when the `@` starts the string or follows whitespace —
/// that is what stops "email@allcorp" and "a@b.com" from reading as mentions,
/// matching the spirit of `mentions_all()` above. Trailing `.`/`-`/`_` are
/// trimmed so "@dana." mentions `dana`, and `@all` is deliberately excluded:
/// it is the everyone-mention, handled by `mentions_all()`, never a username.
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
    /// Only these user ids — a channel message that names specific members by
    /// `@username`. This is the case that lets a mention reach you in a
    /// channel whose ordinary chatter is badge-only.
    Only(Vec<String>),
    /// Nobody — ordinary channel chatter. The accompanying `new_message`
    /// event still bumps the unread badge.
    Nobody,
}

/// Decide the wake-up audience for a message.
///
/// `members` is `(user_id, username)` for every OTHER member of the
/// conversation — the caller reads it from the conversation the sender is
/// already a member of, which is what keeps mentions from becoming a user
/// directory: an `@name` that matches nobody in this room resolves to nothing
/// and reveals nothing. Matching is case-insensitive, and the returned ids
/// preserve `members` order with duplicates removed.
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
    let tokens = mention_tokens(content);
    if tokens.is_empty() {
        return MentionAudience::Nobody;
    }
    let mut ids: Vec<String> = Vec::new();
    for (user_id, username) in members {
        let uname = username.to_lowercase();
        if !uname.is_empty() && tokens.contains(&uname) && !ids.contains(user_id) {
            ids.push(user_id.clone());
        }
    }
    if ids.is_empty() {
        MentionAudience::Nobody
    } else {
        MentionAudience::Only(ids)
    }
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
}

/// A test-only way to make the next outgoing message undecryptable for every
/// recipient (#1312), so a lost message can be produced end to end — through
/// the DS and a real recipient's ingest — instead of only simulated. Compiled
/// out of every shipping build: without `test-harness` it is the identity.
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
