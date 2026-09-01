//! The directory and conversation READ handlers (#987).
//!
//! Every query in this module used to run on the client, against a whole-database
//! read token baked into the shipped binary. Moving them here does three things
//! at once: it removes that credential, it collapses chains of *dependent* round
//! trips into single requests, and it puts each read behind the very
//! authorization predicate the DS already enforces on the write half of the same
//! table — so a read and a write can no longer disagree about who may touch a
//! row.
//!
//! Two rules hold throughout:
//!
//! **Non-membership is data, not an error.** Most handlers answer
//! `authorized: false` with empty payloads rather than 403. That is what the
//! reads they replace did — a membership-joined `SELECT` simply returned no row —
//! and several callers treat it as an ordinary short-circuit. Where a 403 would
//! be an existence oracle (`/v1/messages/lookup`), the refusal is deliberately
//! indistinguishable from absence.
//!
//! **`user_groups` / `user_dms` are never read.** They are the #532 directory
//! index, reverted by #540, and now hold a one-time backfill minus deletions. The
//! DS still DELETEs from them and never inserts, so an endpoint reading them
//! would serve a confidently wrong sidebar. Everything here derives from
//! `group_member` / `dm_channel_member`.

use axum::{
    extract::State,
    response::Response,
};
use std::collections::HashMap;

use libsql::Connection;

use crate::error::AppError;
use crate::groups::group_role;
use crate::reads::authed_user;
use crate::writes::{bad_request, ok_response, RawRequest};
use crate::AppState;
use crate::util::{BIND_CHUNK, placeholders};

pub use pollis_api::directory::*;

/// Cap on how many groups one [`members`] call may name. The batch exists so the
/// command palette opens in one round trip instead of `1 + 2N`; it is not an
/// invitation to enumerate.
const MAX_GROUPS: usize = 128;

// ── POST /v1/conversations/catch-up ──────────────────────────────────────────

/// POST `/v1/conversations/catch-up` — resolve a conversation to its MLS group,
/// enumerate what that group backs, and optionally return the envelopes this
/// device has not fetched.
///
/// This is the request that replaces the longest dependent chain in the client:
/// resolve → membership → is-DM → channel enumerate → envelope fetch, each step
/// needing the previous step's answer, so none of them could be pipelined. Five
/// serial round trips become one.
pub async fn catch_up(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: CatchUpBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match authed_user(&state, &req, parsed.user_id.as_deref()).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };
    let device_id = parsed.device_id.clone().unwrap_or_default();

    let conn = state.db.conn().await?;
    let Some((mls_group_id, kind)) = resolve_conversation(&conn, &parsed.conversation_id).await?
    else {
        // Unknown id. Historically the channel lookup fell through to "treat the
        // id as its own MLS group", which is what a DM id does — so an id that
        // names nothing is reported as unauthorized rather than invented.
        return Ok(ok_response::<CatchUpBody>(CatchUpResponse {
            authorized: false,
            mls_group_id: parsed.conversation_id.clone(),
            kind: ConversationKind::Group,
            conversation_ids: Vec::new(),
            envelopes: Vec::new(),
            dm: None,
            roster: Vec::new(),
        }));
    };

    if !crate::writes::is_member(&conn, &mls_group_id, &who).await? {
        return Ok(ok_response::<CatchUpBody>(CatchUpResponse {
            authorized: false,
            mls_group_id,
            kind,
            conversation_ids: Vec::new(),
            envelopes: Vec::new(),
            dm: None,
            roster: Vec::new(),
        }));
    }

    let conversation_ids = match kind {
        ConversationKind::Dm => vec![mls_group_id.clone()],
        _ => channel_ids_of(&conn, &mls_group_id).await?,
    };

    let envelopes = if parsed.want_envelopes {
        fetch_envelopes(&conn, &conversation_ids, &who, &device_id).await?
    } else {
        Vec::new()
    };

    let dm = if parsed.want_dm && kind == ConversationKind::Dm {
        dm_channel(&conn, &mls_group_id).await?
    } else {
        None
    };

    let roster = if parsed.want_roster {
        desired_roster(&conn, &mls_group_id).await?
    } else {
        Vec::new()
    };

    Ok(ok_response::<CatchUpBody>(CatchUpResponse {
        authorized: true,
        mls_group_id,
        kind,
        conversation_ids,
        envelopes,
        dm,
        roster,
    }))
}

/// The user ids that SHOULD have a leaf in this conversation's MLS tree.
///
/// `group_member` plus pending `group_invite` invitees, falling back to
/// `dm_channel_member` when neither yields anything (a DM has no group rows).
/// Pending invitees are included so their devices get a Welcome at invite time.
///
/// This is THE definition of "desired roster" — `reconcile` diffs the tree
/// against it, `migrate` moves exactly this set into the successor group, and
/// the two must never disagree about who belongs.
pub async fn desired_roster(conn: &Connection, conversation_id: &str) -> anyhow::Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let push = |id: String, out: &mut Vec<String>| {
        if !out.contains(&id) {
            out.push(id);
        }
    };

    let mut rows = conn
        .query(
            "SELECT user_id FROM group_member WHERE group_id = ?1 \
             UNION \
             SELECT invitee_id FROM group_invite WHERE group_id = ?1",
            libsql::params![conversation_id.to_string()],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        push(row.get::<String>(0)?, &mut out);
    }
    drop(rows);

    if out.is_empty() {
        let mut rows = conn
            .query(
                "SELECT user_id FROM dm_channel_member WHERE dm_channel_id = ?1",
                libsql::params![conversation_id.to_string()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            push(row.get::<String>(0)?, &mut out);
        }
    }
    Ok(out)
}

/// Resolve an id in the shared conversation namespace to `(mls_group_id, kind)`.
///
/// The three shapes, in the order the client's own resolve used: a `channels`
/// row maps to its owning group (all of a group's channels share ONE MLS group,
/// keyed by the group id), a `dm_channel` row is its own MLS group, and a
/// `groups` row is addressed directly by the MLS paths.
pub async fn resolve_conversation(
    conn: &Connection,
    id: &str,
) -> anyhow::Result<Option<(String, ConversationKind)>> {
    let mut rows = conn
        .query(
            "SELECT group_id FROM channels WHERE id = ?1",
            libsql::params![id.to_string()],
        )
        .await?;
    if let Some(row) = rows.next().await? {
        return Ok(Some((row.get::<String>(0)?, ConversationKind::Channel)));
    }
    drop(rows);

    let mut rows = conn
        .query(
            "SELECT 1 FROM dm_channel WHERE id = ?1 LIMIT 1",
            libsql::params![id.to_string()],
        )
        .await?;
    if rows.next().await?.is_some() {
        return Ok(Some((id.to_string(), ConversationKind::Dm)));
    }
    drop(rows);

    let mut rows = conn
        .query(
            "SELECT 1 FROM groups WHERE id = ?1 LIMIT 1",
            libsql::params![id.to_string()],
        )
        .await?;
    if rows.next().await?.is_some() {
        return Ok(Some((id.to_string(), ConversationKind::Group)));
    }
    Ok(None)
}

async fn channel_ids_of(conn: &Connection, group_id: &str) -> anyhow::Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT id FROM channels WHERE group_id = ?1",
            libsql::params![group_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(row.get::<String>(0)?);
    }
    Ok(out)
}

/// This device's watermark for each of `conversation_ids`, by conversation.
///
/// Read separately from the envelopes rather than as a subquery inside them —
/// see [`fetch_envelopes`] for why. A conversation with no row is absent from
/// the map and the caller substitutes `""`, which is the same "everything"
/// floor the old `COALESCE` produced.
async fn fetch_watermarks(
    conn: &Connection,
    conversation_ids: &[String],
    user_id: &str,
    device_id: &str,
) -> anyhow::Result<HashMap<String, String>> {
    let sql = format!(
        "SELECT conversation_id, last_fetched_at FROM conversation_watermark \
         WHERE user_id = ?1 AND device_id = ?2 AND conversation_id IN ({})",
        placeholders(conversation_ids.len(), 3)
    );
    let mut params: Vec<libsql::Value> = Vec::with_capacity(conversation_ids.len() + 2);
    params.push(user_id.to_string().into());
    params.push(device_id.to_string().into());
    for cid in conversation_ids {
        params.push(cid.clone().into());
    }
    let mut rows = conn.query(&sql, params).await?;
    let mut out = HashMap::new();
    while let Some(row) = rows.next().await? {
        out.insert(row.get(0)?, row.get(1)?);
    }
    Ok(out)
}

/// Envelopes past each conversation's OWN watermark for this device.
///
/// The bound has to be per conversation — one query serves all of them, and
/// each has its own cursor — but it must also be a value the planner can use as
/// an index RANGE, and those two requirements pull apart.
///
/// Expressing it as a correlated subquery (`WHERE sent_at > (SELECT …  WHERE
/// conversation_id = message_envelope.conversation_id)`) reads correctly and is
/// what this did until now. It cannot use `idx_envelope_channel_time`'s
/// `sent_at` column, because the comparison value is not known until the outer
/// row exists: SQLite plans it `SEARCH … (conversation_id=?)` plus a
/// `CORRELATED SCALAR SUBQUERY`, i.e. **every retained envelope in the
/// conversation is read and then discarded**. Measured on the real schema at
/// 200k envelopes, for a caught-up device returning zero rows: 36 ms and ~200k
/// rows read. Those are billed rows on Turso, on the path every client takes to
/// ask "anything new?" — so the idle case, which should be free, was the
/// expensive one.
///
/// Reading the watermarks first and binding each conversation's own value makes
/// every branch a real range seek (`MULTI-INDEX OR`, each
/// `conversation_id=? AND sent_at>?`): 0.1 ms, no rows read, same one round
/// trip. `pollis-delivery/tests/envelope_query_plan.rs` pins the plan, because
/// this regressed silently once already — #875 batched the per-channel queries
/// into one and turned a bound `?1` into a correlation without anything noticing.
///
/// Splitting the read in two is safe in the only direction that matters. The
/// watermark for `(conversation, user, device)` is advanced by this device
/// alone, so a concurrent advance between the two reads can only leave this
/// query using an OLDER cursor — re-sending an envelope the client already has,
/// which `ingest.rs`'s `INSERT OR IGNORE INTO message` absorbs. It can never
/// skip one.
async fn fetch_envelopes(
    conn: &Connection,
    conversation_ids: &[String],
    user_id: &str,
    device_id: &str,
) -> anyhow::Result<Vec<EnvelopeWire>> {
    let mut out = Vec::new();
    // Two binds per conversation now (its id and its own watermark), so half as
    // many conversations fit under the same variable ceiling.
    for chunk in conversation_ids.chunks(BIND_CHUNK / 2) {
        let watermarks = fetch_watermarks(conn, chunk, user_id, device_id).await?;
        let mut clauses = Vec::with_capacity(chunk.len());
        let mut params: Vec<libsql::Value> = Vec::with_capacity(chunk.len() * 2);
        for (i, cid) in chunk.iter().enumerate() {
            clauses.push(format!(
                "(conversation_id = ?{} AND sent_at > ?{})",
                i * 2 + 1,
                i * 2 + 2
            ));
            params.push(cid.clone().into());
            params.push(watermarks.get(cid).cloned().unwrap_or_default().into());
        }
        let sql = format!(
            "SELECT conversation_id, id, sender_id, ciphertext, reply_to_id, \
                    target_message_id, sent_at, type \
             FROM message_envelope \
             WHERE {} \
             ORDER BY conversation_id ASC, sent_at ASC, id ASC",
            clauses.join(" OR ")
        );
        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            out.push(EnvelopeWire {
                conversation_id: row.get(0)?,
                id: row.get(1)?,
                sender_id: row.get(2)?,
                ciphertext: row.get(3)?,
                reply_to_id: row.get(4)?,
                target_message_id: row.get(5)?,
                sent_at: row.get(6)?,
                kind: row.get(7)?,
            });
        }
    }
    Ok(out)
}

/// One DM channel with its members. The caller's membership is already
/// established by the time this runs.
async fn dm_channel(
    conn: &Connection,
    dm_channel_id: &str,
) -> anyhow::Result<Option<DmChannelWire>> {
    let mut rows = conn
        .query(
            "SELECT id, created_by, created_at FROM dm_channel WHERE id = ?1",
            libsql::params![dm_channel_id.to_string()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let (id, created_by, created_at): (String, String, String) =
        (row.get(0)?, row.get(1)?, row.get(2)?);
    drop(rows);
    Ok(Some(DmChannelWire {
        members: dm_members(conn, &id).await?,
        id,
        created_by,
        created_at,
    }))
}

// ── POST /v1/messages/lookup ─────────────────────────────────────────────────

/// POST `/v1/messages/lookup` — which conversation an envelope belongs to.
///
/// Answers `None` both for "no such envelope" and for "you are not a member of
/// its conversation". Distinguishing them would turn this into an existence
/// oracle for message ids, and the caller cannot act on either answer.
pub async fn message_lookup(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: MessageLookupBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match authed_user(&state, &req, parsed.user_id.as_deref()).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };

    let conn = state.db.conn().await?;
    let conversation_id = if parsed.message_id.is_empty() {
        None
    } else {
        envelope_conversation(&conn, &parsed.message_id, &who).await?
    };

    // Reactions are gated on the same question — membership of the message's
    // conversation — asked once per message. A message the caller may not see
    // is simply absent, never a distinguishable refusal.
    let mut reactions = Vec::new();
    for message_id in &parsed.reactions_for {
        let Some(_) = envelope_conversation(&conn, message_id, &who).await? else {
            continue;
        };
        let grouped = message_reactions(&conn, message_id).await?;
        if !grouped.is_empty() {
            reactions.push(MessageReactions {
                message_id: message_id.clone(),
                reactions: grouped,
            });
        }
    }

    Ok(ok_response::<MessageLookupBody>(MessageLookupResponse {
        conversation_id,
        reactions,
    }))
}

/// The conversation an envelope belongs to, or `None` when it does not exist OR
/// the caller is not a member of it.
///
/// The two cases are deliberately indistinguishable: separating them would make
/// this an existence oracle for message ids, and no caller can act on the
/// difference.
async fn envelope_conversation(
    conn: &Connection,
    message_id: &str,
    user_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT conversation_id FROM message_envelope WHERE id = ?1 AND type = 'message'",
            libsql::params![message_id.to_string()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let cid: String = row.get(0)?;
    drop(rows);
    if crate::writes::is_member(conn, &cid, user_id).await? {
        Ok(Some(cid))
    } else {
        Ok(None)
    }
}

/// One message's reactions, grouped by emoji in insertion order.
async fn message_reactions(
    conn: &Connection,
    message_id: &str,
) -> anyhow::Result<Vec<ReactionWire>> {
    let mut rows = conn
        .query(
            "SELECT emoji, user_id FROM message_reaction \
             WHERE message_id = ?1 ORDER BY created_at ASC",
            libsql::params![message_id.to_string()],
        )
        .await?;
    let mut out: Vec<ReactionWire> = Vec::new();
    while let Some(row) = rows.next().await? {
        let emoji: String = row.get(0)?;
        let user_id: String = row.get(1)?;
        match out.iter_mut().find(|r| r.emoji == emoji) {
            Some(existing) => existing.user_ids.push(user_id),
            None => out.push(ReactionWire {
                emoji,
                user_ids: vec![user_id],
            }),
        }
    }
    Ok(out)
}

// ── POST /v1/directory/bootstrap ─────────────────────────────────────────────

/// POST `/v1/directory/bootstrap` — the whole sidebar, actor-scoped, in one
/// request.
pub async fn bootstrap(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: DirectoryBootstrapBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match authed_user(&state, &req, parsed.user_id.as_deref()).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };

    let conn = state.db.conn().await?;
    Ok(ok_response::<DirectoryBootstrapBody>(
        DirectoryBootstrapResponse {
            groups: groups_with_channels(&conn, &who).await?,
            dms: dm_channels(&conn, &who, true).await?,
            dm_requests: dm_channels(&conn, &who, false).await?,
            pending_invites: pending_invites(&conn, &who).await?,
            blocks: blocked_users(&conn, &who).await?,
            preferences: preferences(&conn, &who).await?,
        },
    ))
}

/// Every group the caller belongs to, each carrying its channels and the
/// caller's role. One `LEFT JOIN` so a group with no channels still appears.
pub async fn groups_with_channels(
    conn: &Connection,
    user_id: &str,
) -> anyhow::Result<Vec<GroupWithChannelsWire>> {
    let mut rows = conn
        .query(
            "SELECT g.id, g.name, g.description, g.owner_id, g.created_at, \
                    c.id, c.group_id, c.name, c.description, c.channel_type, gm.role \
             FROM groups g \
             JOIN group_member gm ON gm.group_id = g.id \
             LEFT JOIN channels c ON c.group_id = g.id \
             WHERE gm.user_id = ?1 \
             ORDER BY g.created_at, c.name",
            libsql::params![user_id.to_string()],
        )
        .await?;

    let mut out: Vec<GroupWithChannelsWire> = Vec::new();
    while let Some(row) = rows.next().await? {
        let group_id: String = row.get(0)?;
        let channel_id: Option<String> = row.get(5)?;
        let channel = match channel_id {
            Some(cid) => Some(ChannelWire {
                id: cid,
                group_id: row.get(6)?,
                name: row.get(7)?,
                description: row.get(8)?,
                channel_type: row
                    .get::<Option<String>>(9)?
                    .unwrap_or_else(|| "text".to_string()),
            }),
            None => None,
        };
        match out.iter_mut().find(|g| g.group.id == group_id) {
            Some(existing) => {
                if let Some(c) = channel {
                    existing.channels.push(c);
                }
            }
            None => out.push(GroupWithChannelsWire {
                group: GroupWire {
                    id: group_id,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    owner_id: row.get(3)?,
                    created_at: row.get(4)?,
                },
                channels: channel.into_iter().collect(),
                role: row.get(10)?,
            }),
        }
    }
    Ok(out)
}

/// The caller's DMs, `accepted = true` for the sidebar list and `false` for the
/// request list.
///
/// The block filter is deliberately ASYMMETRIC and must stay so: it hides
/// channels whose other members the caller has blocked, but NOT channels where
/// the caller is the blocked party. A blocked user keeps seeing the
/// conversation, so the block produces no observable signal on their side —
/// their messages simply stay pending, indistinguishable from a recipient who
/// has not accepted.
pub async fn dm_channels(
    conn: &Connection,
    user_id: &str,
    accepted: bool,
) -> anyhow::Result<Vec<DmChannelWire>> {
    let accepted_clause = if accepted {
        "dcm.accepted_at IS NOT NULL"
    } else {
        "dcm.accepted_at IS NULL"
    };
    let sql = format!(
        "SELECT dc.id, dc.created_by, dc.created_at \
         FROM dm_channel dc \
         JOIN dm_channel_member dcm ON dcm.dm_channel_id = dc.id \
         WHERE dcm.user_id = ?1 \
           AND {accepted_clause} \
           AND NOT EXISTS ( \
             SELECT 1 FROM dm_channel_member other \
             JOIN user_block ub ON ub.blocker_id = ?1 AND ub.blocked_id = other.user_id \
             WHERE other.dm_channel_id = dc.id AND other.user_id <> ?1 \
           ) \
         ORDER BY dc.created_at DESC"
    );
    let mut rows = conn
        .query(&sql, libsql::params![user_id.to_string()])
        .await?;
    let mut shells: Vec<(String, String, String)> = Vec::new();
    while let Some(row) = rows.next().await? {
        shells.push((row.get(0)?, row.get(1)?, row.get(2)?));
    }
    drop(rows);

    let mut out = Vec::with_capacity(shells.len());
    for (id, created_by, created_at) in shells {
        let members = dm_members(conn, &id).await?;
        out.push(DmChannelWire {
            id,
            created_by,
            created_at,
            members,
        });
    }
    Ok(out)
}

/// One DM as `/v1/dm/create` answers with it.
pub async fn dm_row(
    conn: &Connection,
    dm_id: &str,
    existing: bool,
) -> anyhow::Result<Option<pollis_api::profile::CreatedDm>> {
    let mut rows = conn
        .query(
            "SELECT id, created_by, created_at FROM dm_channel WHERE id = ?1",
            libsql::params![dm_id.to_string()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let (id, created_by, created_at): (String, String, String) =
        (row.get(0)?, row.get(1)?, row.get(2)?);
    drop(rows);
    let members = dm_members(conn, &id)
        .await?
        .into_iter()
        .map(|m| pollis_api::profile::DmMember {
            user_id: m.user_id,
            username: m.username,
            avatar_url: m.avatar_url,
            accepted_at: m.accepted_at,
        })
        .collect();
    Ok(Some(pollis_api::profile::CreatedDm::Ok {
        id,
        created_by,
        created_at,
        members,
        existing,
    }))
}

pub async fn dm_members(conn: &Connection, dm_id: &str) -> anyhow::Result<Vec<DmMemberWire>> {
    let mut rows = conn
        .query(
            "SELECT dcm.user_id, u.username, u.avatar_url, dcm.added_by, dcm.added_at, \
                    dcm.accepted_at \
             FROM dm_channel_member dcm \
             LEFT JOIN users u ON u.id = dcm.user_id \
             WHERE dcm.dm_channel_id = ?1",
            libsql::params![dm_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(DmMemberWire {
            user_id: row.get(0)?,
            username: row.get(1)?,
            avatar_url: row.get(2)?,
            added_by: row.get(3)?,
            added_at: row.get(4)?,
            accepted_at: row.get(5)?,
        });
    }
    Ok(out)
}

async fn pending_invites(
    conn: &Connection,
    user_id: &str,
) -> anyhow::Result<Vec<PendingInviteWire>> {
    let mut rows = conn
        .query(
            "SELECT gi.id, gi.group_id, g.name, gi.inviter_id, u.username, gi.created_at \
             FROM group_invite gi \
             JOIN groups g ON g.id = gi.group_id \
             LEFT JOIN users u ON u.id = gi.inviter_id \
             WHERE gi.invitee_id = ?1 \
             ORDER BY gi.created_at DESC",
            libsql::params![user_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(PendingInviteWire {
            id: row.get(0)?,
            group_id: row.get(1)?,
            group_name: row.get(2)?,
            inviter_id: row.get(3)?,
            inviter_username: row.get(4)?,
            created_at: row.get(5)?,
        });
    }
    Ok(out)
}

async fn blocked_users(conn: &Connection, user_id: &str) -> anyhow::Result<Vec<BlockedUserWire>> {
    let mut rows = conn
        .query(
            "SELECT ub.blocked_id, u.username, ub.created_at \
             FROM user_block ub \
             LEFT JOIN users u ON u.id = ub.blocked_id \
             WHERE ub.blocker_id = ?1 \
             ORDER BY ub.created_at DESC",
            libsql::params![user_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(BlockedUserWire {
            user_id: row.get(0)?,
            username: row.get(1)?,
            blocked_at: row.get(2)?,
        });
    }
    Ok(out)
}

async fn preferences(conn: &Connection, user_id: &str) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT preferences FROM user_preferences WHERE user_id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row.get::<String>(0)?)),
        None => Ok(None),
    }
}

// ── POST /v1/directory/group ─────────────────────────────────────────────────

/// POST `/v1/directory/group` — one group's detail, gated per section.
///
/// Members see the roster, the channels and (on request) the emoji set. Invite
/// links and join requests are admin-only, and an unauthorized section comes back
/// EMPTY rather than as a refusal for the whole request — the member half is
/// still legitimately theirs.
pub async fn group(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let parsed: DirectoryGroupBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match authed_user(&state, &req, parsed.user_id.as_deref()).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };

    let conn = state.db.conn().await?;
    // The id may name a group or one of its channels. Resolving here is what
    // lets the moderation preflight ask one question instead of two, and it
    // matches `writes::is_member`'s tolerance of the shared id namespace.
    let group_id = match resolve_conversation(&conn, &parsed.group_id).await? {
        Some((id, ConversationKind::Group)) | Some((id, ConversationKind::Channel)) => id,
        // A DM, or nothing at all. Neither is a group, and `exists: false` is
        // the honest answer to "is this a group I could be in".
        _ => {
            return Ok(ok_response::<DirectoryGroupBody>(DirectoryGroupResponse {
                group_id: parsed.group_id.clone(),
                exists: false,
                authorized: false,
                role: None,
                group: None,
                channels: Vec::new(),
                members: Vec::new(),
                invite_links: Vec::new(),
                join_requests: Vec::new(),
                emoji: Vec::new(),
            }))
        }
    };

    let role = group_role(&conn, &group_id, &who).await?;
    let Some(role) = role else {
        return Ok(ok_response::<DirectoryGroupBody>(DirectoryGroupResponse {
            group_id,
            exists: true,
            authorized: false,
            role: None,
            group: None,
            channels: Vec::new(),
            members: Vec::new(),
            invite_links: Vec::new(),
            join_requests: Vec::new(),
            emoji: Vec::new(),
        }));
    };
    let admin = role == "admin";
    if parsed.role_only {
        return Ok(ok_response::<DirectoryGroupBody>(DirectoryGroupResponse {
            group_id,
            exists: true,
            authorized: true,
            role: Some(role),
            group: None,
            channels: Vec::new(),
            members: Vec::new(),
            invite_links: Vec::new(),
            join_requests: Vec::new(),
            emoji: Vec::new(),
        }));
    }

    Ok(ok_response::<DirectoryGroupBody>(DirectoryGroupResponse {
        exists: true,
        authorized: true,
        group: group_row(&conn, &group_id).await?,
        group_id: group_id.clone(),
        channels: channels_of(&conn, &group_id).await?,
        members: group_members(&conn, &group_id).await?,
        invite_links: if admin {
            invite_links(&conn, &group_id).await?
        } else {
            Vec::new()
        },
        join_requests: if admin {
            join_requests(&conn, &group_id).await?
        } else {
            Vec::new()
        },
        emoji: if parsed.want_emoji {
            group_emoji(&conn, &group_id).await?
        } else {
            Vec::new()
        },
        role: Some(role),
    }))
}

pub async fn group_row(conn: &Connection, group_id: &str) -> anyhow::Result<Option<GroupWire>> {
    let mut rows = conn
        .query(
            "SELECT id, name, description, owner_id, created_at FROM groups WHERE id = ?1",
            libsql::params![group_id.to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(GroupWire {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            owner_id: row.get(3)?,
            created_at: row.get(4)?,
        })),
        None => Ok(None),
    }
}

async fn channels_of(conn: &Connection, group_id: &str) -> anyhow::Result<Vec<ChannelWire>> {
    let mut rows = conn
        .query(
            "SELECT id, group_id, name, description, channel_type FROM channels WHERE group_id = ?1",
            libsql::params![group_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(ChannelWire {
            id: row.get(0)?,
            group_id: row.get(1)?,
            name: row.get(2)?,
            description: row.get(3)?,
            channel_type: row
                .get::<Option<String>>(4)?
                .unwrap_or_else(|| "text".to_string()),
        });
    }
    Ok(out)
}

pub async fn group_members(
    conn: &Connection,
    group_id: &str,
) -> anyhow::Result<Vec<GroupMemberWire>> {
    let mut rows = conn
        .query(
            "SELECT gm.user_id, u.username, u.avatar_url, gm.role, gm.joined_at \
             FROM group_member gm \
             LEFT JOIN users u ON u.id = gm.user_id \
             WHERE gm.group_id = ?1",
            libsql::params![group_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(GroupMemberWire {
            user_id: row.get(0)?,
            username: row.get(1)?,
            avatar_url: row.get(2)?,
            role: row.get(3)?,
            joined_at: row.get(4)?,
        });
    }
    Ok(out)
}

/// `is_live` is computed by SQLite with the same `datetime()` normalisation the
/// redeem path uses, so the badge cannot disagree with what redemption will
/// actually do.
async fn invite_links(conn: &Connection, group_id: &str) -> anyhow::Result<Vec<InviteLinkWire>> {
    let mut rows = conn
        .query(
            "SELECT l.id, l.created_at, u.username, l.expires_at, l.max_uses, l.uses, \
                    l.revoked_at, \
                    CASE WHEN l.revoked_at IS NULL \
                          AND (l.expires_at IS NULL OR datetime(l.expires_at) > datetime('now')) \
                          AND (l.max_uses IS NULL OR l.uses < l.max_uses) \
                         THEN 1 ELSE 0 END AS is_live \
             FROM group_invite_link l \
             LEFT JOIN users u ON u.id = l.created_by \
             WHERE l.group_id = ?1 \
             ORDER BY l.created_at DESC",
            libsql::params![group_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(InviteLinkWire {
            id: row.get(0)?,
            created_at: row.get(1)?,
            creator_username: row.get(2)?,
            expires_at: row.get(3)?,
            max_uses: row.get(4)?,
            uses: row.get(5)?,
            revoked_at: row.get(6)?,
            is_live: row.get::<i64>(7)? != 0,
        });
    }
    Ok(out)
}

async fn join_requests(conn: &Connection, group_id: &str) -> anyhow::Result<Vec<JoinRequestWire>> {
    let mut rows = conn
        .query(
            "SELECT jr.id, jr.group_id, jr.requester_id, u.username, jr.status, jr.created_at \
             FROM group_join_request jr \
             LEFT JOIN users u ON u.id = jr.requester_id \
             WHERE jr.group_id = ?1 AND jr.status = 'pending' \
             ORDER BY jr.created_at ASC",
            libsql::params![group_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(JoinRequestWire {
            id: row.get(0)?,
            group_id: row.get(1)?,
            requester_id: row.get(2)?,
            requester_username: row.get(3)?,
            status: row.get(4)?,
            created_at: row.get(5)?,
        });
    }
    Ok(out)
}

pub async fn group_emoji(
    conn: &Connection,
    group_id: &str,
) -> anyhow::Result<Vec<CustomEmojiWire>> {
    let mut rows = conn
        .query(
            "SELECT ge.group_id, COALESCE(g.name, ''), ge.shortcode, ge.content_hash, \
                    o.content_type, o.animated, o.size_bytes, ge.created_by \
             FROM group_emoji ge \
             JOIN custom_emoji_object o ON o.content_hash = ge.content_hash \
             LEFT JOIN groups g ON g.id = ge.group_id \
             WHERE ge.group_id = ?1 \
             ORDER BY ge.shortcode",
            libsql::params![group_id.to_string()],
        )
        .await?;
    collect_emoji(&mut rows).await
}

pub async fn collect_emoji(rows: &mut libsql::Rows) -> anyhow::Result<Vec<CustomEmojiWire>> {
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(CustomEmojiWire {
            group_id: row.get(0)?,
            group_name: row.get(1)?,
            shortcode: row.get(2)?,
            content_hash: row.get(3)?,
            content_type: row.get(4)?,
            animated: row.get::<i64>(5)? != 0,
            size_bytes: row.get::<i64>(6)?.max(0),
            created_by: row.get(7)?,
        });
    }
    Ok(out)
}

// ── POST /v1/directory/members ───────────────────────────────────────────────

/// POST `/v1/directory/members` — member lists for several groups at once.
///
/// Each group is authorized independently, so one group the caller has left does
/// not fail the batch. That matters because the caller is the command palette,
/// whose group list can be a moment stale.
pub async fn members(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let parsed: DirectoryMembersBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match authed_user(&state, &req, parsed.user_id.as_deref()).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };
    if parsed.group_ids.len() > MAX_GROUPS {
        return Ok(bad_request("too many groups"));
    }

    let conn = state.db.conn().await?;
    let mut groups = Vec::with_capacity(parsed.group_ids.len());
    for group_id in &parsed.group_ids {
        let authorized = crate::groups::is_member(&conn, group_id, &who).await?;
        groups.push(GroupMembersWire {
            group_id: group_id.clone(),
            authorized,
            members: if authorized {
                group_members(&conn, group_id).await?
            } else {
                Vec::new()
            },
        });
    }
    Ok(ok_response::<DirectoryMembersBody>(
        DirectoryMembersResponse { groups },
    ))
}

// ── POST /v1/directory/users ─────────────────────────────────────────────────

/// POST `/v1/directory/users` — profile hydration and username/email lookup.
///
/// The projection deliberately omits `phone`: nothing renders it, and it is the
/// most re-identifying column on the row, so it does not cross this boundary at
/// all rather than crossing it and being ignored.
pub async fn users(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let parsed: DirectoryUsersBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    // The block check is answered for the AUTHENTICATED caller, never for a
    // body-named one: "is A blocked by B" for arbitrary A and B would publish
    // the block graph.
    let who = match authed_user(&state, &req, None).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };

    let conn = state.db.conn().await?;
    let mut out: Vec<UserWire> = Vec::new();

    for chunk in parsed.user_ids.chunks(BIND_CHUNK) {
        let sql = format!(
            "SELECT id, username, preferred_name, avatar_url FROM users WHERE id IN ({})",
            placeholders(chunk.len(), 1)
        );
        let params: Vec<libsql::Value> = chunk.iter().map(|id| id.clone().into()).collect();
        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            out.push(UserWire {
                id: row.get(0)?,
                username: row.get(1)?,
                preferred_name: row.get(2)?,
                avatar_url: row.get(3)?,
            });
        }
    }

    if let Some(identifier) = parsed.identifier.as_deref().filter(|s| !s.is_empty()) {
        let mut rows = conn
            .query(
                "SELECT id, username, preferred_name, avatar_url \
                 FROM users WHERE username = ?1 OR email = ?1",
                libsql::params![identifier.to_string()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let user = UserWire {
                id: row.get(0)?,
                username: row.get(1)?,
                preferred_name: row.get(2)?,
                avatar_url: row.get(3)?,
            };
            if !out.iter().any(|u| u.id == user.id) {
                out.push(user);
            }
        }
    }

    let mut blocked = Vec::new();
    for chunk in parsed.block_check.chunks(BIND_CHUNK) {
        let sql = format!(
            "SELECT CASE WHEN blocker_id = ?1 THEN blocked_id ELSE blocker_id END \
             FROM user_block \
             WHERE (blocker_id = ?1 AND blocked_id IN ({0})) \
                OR (blocked_id = ?1 AND blocker_id IN ({0}))",
            placeholders(chunk.len(), 2)
        );
        let mut params: Vec<libsql::Value> = Vec::with_capacity(chunk.len() + 1);
        params.push(who.clone().into());
        for id in chunk {
            params.push(id.clone().into());
        }
        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            let other: String = row.get(0)?;
            if !blocked.contains(&other) {
                blocked.push(other);
            }
        }
    }

    Ok(ok_response::<DirectoryUsersBody>(DirectoryUsersResponse {
        users: out,
        blocked,
    }))
}

// ── POST /v1/directory/group-by-slug ─────────────────────────────────────────

/// POST `/v1/directory/group-by-slug` — the join flow's discovery step.
///
/// Deliberately not membership-gated (#917) and, unlike every other endpoint
/// here, reachable by any authenticated caller for any group: gating it would
/// mean you could only find groups you were already in, which removes the
/// feature rather than tightening it. What protects it is the projection —
/// [`GroupPreview`] carries no `owner_id` — plus the per-IP rate limit the router
/// applies, because guessing slugs is the only way to use it.
pub async fn group_by_slug(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: GroupBySlugBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    if let Err(resp) = authed_user(&state, &req, None).await? {
        return Ok(resp);
    }

    let target = parsed.slug.trim().to_lowercase();
    let conn = state.db.conn().await?;
    // Only the three columns the preview carries are SELECTed. Narrowing the
    // query as well as the struct means a future edit to the mapping cannot
    // reach a sensitive column without also editing the SQL.
    let mut rows = conn
        .query("SELECT id, name, description FROM groups", ())
        .await?;
    let mut found = None;
    while let Some(row) = rows.next().await? {
        let name: String = row.get(1)?;
        if derive_slug(&name) == target {
            found = Some(GroupPreview {
                id: row.get(0)?,
                name,
                description: row.get(2)?,
            });
            break;
        }
    }
    Ok(ok_response::<GroupBySlugBody>(GroupBySlugResponse {
        group: found,
    }))
}

// ── POST /v1/directory/conversations ─────────────────────────────────────────

/// POST `/v1/directory/conversations` — id-only enumeration of everything the
/// caller participates in.
pub async fn conversations(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: DirectoryConversationsBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match authed_user(&state, &req, parsed.user_id.as_deref()).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };

    let conn = state.db.conn().await?;
    Ok(ok_response::<DirectoryConversationsBody>(
        DirectoryConversationsResponse {
            group_ids: user_group_ids(&conn, &who).await?,
            dm_ids: user_dm_ids(&conn, &who).await?,
            dm_with: match parsed.dm_with_user_id.as_deref() {
                Some(other) => one_to_one_dm(&conn, &who, other).await?,
                None => None,
            },
        },
    ))
}

/// The 1:1 DM channel shared by two users, if any.
///
/// `HAVING COUNT(*) = 2` guards against a group DM accidentally matching — a
/// three-person DM containing both users is not the 1:1 channel, and using it
/// for a call's MLS group would export a key to a third party. Ordered by id so
/// the answer is deterministic if two ever existed.
async fn one_to_one_dm(
    conn: &Connection,
    user_id: &str,
    other_user_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT dcm.dm_channel_id \
             FROM dm_channel_member dcm \
             WHERE dcm.dm_channel_id IN ( \
                 SELECT dm_channel_id FROM dm_channel_member WHERE user_id = ?1 \
             ) \
             AND dcm.user_id = ?2 \
             AND ( \
                 SELECT COUNT(*) FROM dm_channel_member \
                 WHERE dm_channel_id = dcm.dm_channel_id \
             ) = 2 \
             ORDER BY dcm.dm_channel_id LIMIT 1",
            libsql::params![user_id.to_string(), other_user_id.to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row.get::<String>(0)?)),
        None => Ok(None),
    }
}

pub async fn user_group_ids(conn: &Connection, user_id: &str) -> anyhow::Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT g.id FROM groups g \
             JOIN group_member gm ON gm.group_id = g.id \
             WHERE gm.user_id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(row.get::<String>(0)?);
    }
    Ok(out)
}

pub async fn user_dm_ids(conn: &Connection, user_id: &str) -> anyhow::Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT dm_channel_id FROM dm_channel_member WHERE user_id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(row.get::<String>(0)?);
    }
    Ok(out)
}

/// Every emoji the caller may USE — the picker's source and the send-permission
/// rule in one query, which is the point: the set the UI offers and the set the
/// rule permits cannot drift because they are the same set.
///
/// Discord's rule (#848): membership of ANY registering group grants use in ANY
/// conversation. Note the absence of a conversation argument — scoping this to
/// the conversation is the obvious-looking rule and the wrong one.
pub async fn usable_emoji(
    conn: &Connection,
    user_id: &str,
) -> anyhow::Result<Vec<CustomEmojiWire>> {
    let mut rows = conn
        .query(
            "SELECT ge.group_id, COALESCE(g.name, ''), ge.shortcode, ge.content_hash, \
                    o.content_type, o.animated, o.size_bytes, ge.created_by \
             FROM group_emoji ge \
             JOIN group_member gm ON gm.group_id = ge.group_id AND gm.user_id = ?1 \
             JOIN custom_emoji_object o ON o.content_hash = ge.content_hash \
             LEFT JOIN groups g ON g.id = ge.group_id \
             ORDER BY g.name COLLATE NOCASE, ge.shortcode",
            libsql::params![user_id.to_string()],
        )
        .await?;
    collect_emoji(&mut rows).await
}

