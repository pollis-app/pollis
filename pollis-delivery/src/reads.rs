//! The MLS control plane's read half (#987).
//!
//! [`conversation_state`] answers each conversation from one log-DB
//! transaction, so a device never sees a GroupInfo without the Welcome written
//! beside it, nor a torn commit batch (see `pollis_api::reads`).
//!
//! Reads are signed POSTs, never GETs: the signing message excludes the query
//! string (#681), and GETs skip rate limiting and `pollis_api::ENDPOINTS`.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
};
use libsql::Connection;

use crate::auth;
use crate::error::{AppError, AuthRejection};
use crate::writes::{bad_request, is_member, ok_response, RawRequest};
use crate::AppState;
use crate::util::{BIND_CHUNK, b64, placeholders};

pub use pollis_api::reads::*;

/// Cap on conversations per [`conversation_state`] call. Entries are
/// independent, so a caller with more pages at no cost to correctness.
pub const MAX_QUERIES: usize = 64;

/// The authenticated `(user_id, device_id)` a read is answered for.
///
/// With auth on, both come from the verified signature, never the body: a
/// body `device_id` would let a user drain another device's Welcome queue.
pub struct Reader {
    pub user_id: String,
    pub device_id: String,
}

/// [`reader`] for sibling modules' device-scoped reads.
// The `Err` IS the response returned to the client, as elsewhere in this crate.
#[allow(clippy::result_large_err)]
pub(crate) async fn reader_identity(
    state: &AppState,
    req: &RawRequest,
) -> Result<Result<Reader, Response>, AppError> {
    reader(state, req, None, None).await
}

/// Authenticate a read and resolve the reader identity; body fields only on the
/// no-auth path.
// The `Err` IS the response returned to the client, as elsewhere in this crate.
#[allow(clippy::result_large_err)]
async fn reader(
    state: &AppState,
    req: &RawRequest,
    body_user_id: Option<&str>,
    body_device_id: Option<&str>,
) -> Result<Result<Reader, Response>, AppError> {
    if !state.require_auth {
        return Ok(match (body_user_id, body_device_id) {
            (Some(u), Some(d)) if !u.is_empty() && !d.is_empty() => Ok(Reader {
                user_id: u.to_string(),
                device_id: d.to_string(),
            }),
            _ => Err(bad_request(
                "user_id and device_id required when auth is disabled",
            )),
        });
    }
    match auth::verify_request_identity_cached(
        &state.device_keys,
        &state.db,
        &req.headers,
        req.method.as_str(),
        req.uri.path(),
        &req.body,
        crate::util::now_unix() as i64,
    )
    .await
    {
        Ok((user_id, device_id)) => Ok(Ok(Reader { user_id, device_id })),
        Err(rej) => Ok(Err(rej.into_response())),
    }
}

/// Authenticate a read that needs only the user half of the identity. With auth
/// on, a body naming a different user is a 403, not an ignored field.
// The `Err` IS the response returned to the client, as elsewhere in this crate.
#[allow(clippy::result_large_err)]
pub(crate) async fn authed_user(
    state: &AppState,
    req: &RawRequest,
    body_user_id: Option<&str>,
) -> Result<Result<String, Response>, AppError> {
    if !state.require_auth {
        return Ok(match body_user_id {
            Some(u) if !u.is_empty() => Ok(u.to_string()),
            _ => Err(bad_request("user_id required when auth is disabled")),
        });
    }
    match auth::verify_request_cached(
        &state.device_keys,
        &state.db,
        &req.headers,
        req.method.as_str(),
        req.uri.path(),
        &req.body,
        crate::util::now_unix() as i64,
    )
    .await
    {
        Ok(user_id) => {
            if let Some(claimed) = body_user_id {
                if !claimed.is_empty() && claimed != user_id {
                    return Ok(Err(AuthRejection::Forbidden.into_response()));
                }
            }
            Ok(Ok(user_id))
        }
        Err(rej) => Ok(Err(rej.into_response())),
    }
}

/// POST `/v1/mls/conversation-state` — the inputs the client's MLS replay and
/// join recovery need, batched across conversations.
///
/// Authorization is per entry, so one conversation the caller was removed from
/// does not blind it to the rest; non-membership looks like "no state".
pub async fn conversation_state(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: ConversationStateBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who =
        match reader(&state, &req, parsed.user_id.as_deref(), parsed.device_id.as_deref()).await? {
            Ok(r) => r,
            Err(resp) => return Ok(resp),
        };
    if parsed.queries.len() > MAX_QUERIES {
        return Ok(bad_request("too many queries"));
    }

    let main = state.db.conn().await?;
    let log = state.log_db.conn().await?;

    let mut states = Vec::with_capacity(parsed.queries.len());
    for q in &parsed.queries {
        // Also reported as `is_member`, an input to the client's fail-closed
        // external-join gate.
        let member = is_member(&main, &q.conversation_id, &who.user_id).await;
        let authorized = matches!(member, Ok(true));
        if !authorized {
            states.push(ConversationState {
                conversation_id: q.conversation_id.clone(),
                authorized: false,
                group_info: None,
                welcome_pending: false,
                head: 0,
                head_generation: 0,
                generation: q.generation.unwrap_or(0),
                commits: Vec::new(),
                hole: None,
                pruned_below: None,
                commit_at_epoch: None,
                // A failed read is `None`, not `Some(false)`, so a sick main DB
                // does not masquerade as "you were removed".
                is_member: member.as_ref().ok().copied(),
                device_registered: None,
                device_registered_at: None,
                member_since: None,
                added_identities: Vec::new(),
            });
            continue;
        }
        let registered = device_registered(&main, &who.user_id, &who.device_id)
            .await
            .ok();
        let mut st = snapshot(&log, q, &who, registered).await?;
        // Best-effort (#1571): a failed read must never fail the snapshot.
        st.device_registered_at = device_registered_at(&main, &who.user_id, &who.device_id)
            .await
            .ok()
            .flatten();
        st.member_since = member_since(&main, &q.conversation_id, &who.user_id)
            .await
            .ok()
            .flatten();
        // Main-DB rows, outside the log snapshot; the client verifies them by
        // signature, so no atomicity with the log is needed.
        st.added_identities = added_identities(&main, &st.commits).await?;
        states.push(st);
    }

    Ok(ok_response::<ConversationStateBody>(
        ConversationStateResponse { states },
    ))
}

/// One conversation's log-DB snapshot, read inside one transaction.
///
/// `submit_commit` writes commit, GroupInfo and Welcomes atomically, so seeing
/// the GroupInfo implies seeing the Welcome (the client's `ExternalJoin ⟹
/// !welcome_pending` gate), and `head` / `commits` cannot disagree into a
/// phantom gap. `pub` for `tests/conversation_snapshot.rs`.
pub async fn snapshot(
    log: &Connection,
    q: &ConversationStateQuery,
    who: &Reader,
    device_registered: Option<bool>,
) -> Result<ConversationState, AppError> {
    let tx = log.transaction().await?;

    let head_generation = crate::commit::head_generation(&tx, &q.conversation_id).await?;
    let generation = q.generation.unwrap_or(head_generation);
    let head = crate::commit::head_epoch_in(&tx, &q.conversation_id, generation).await?;

    let group_info = read_group_info(&tx, &q.conversation_id, q.want_group_info_blob).await?;

    // After the GroupInfo read, as the client's gate requires.
    let welcome_pending =
        welcome_pending(&tx, &q.conversation_id, &who.user_id, &who.device_id).await?;

    let pruned_below = min_epoch_in(&tx, &q.conversation_id, generation).await?;

    let commits = match q.since {
        Some(since) => {
            crate::commit::fetch_commits(&tx, &q.conversation_id, generation, since).await?
        }
        None => Vec::new(),
    };

    let commit_at_epoch = match q.commit_at_epoch {
        Some(epoch) => commit_at(&tx, &q.conversation_id, generation, epoch)
            .await?
            .map(|b| b64(&b)),
        None => None,
    };

    tx.commit().await?;

    // Report a hole; do not refuse the batch. The client's recovery is to drop
    // the local group and external-join; erroring would leave a damaged
    // conversation unreadable forever. One transaction means the hole is real.
    let hole = check_contiguous(&commits).err();
    if let Some(h) = hole {
        tracing::warn!(
            conversation_id = %q.conversation_id,
            generation,
            expected = h.expected,
            found = h.found,
            "commit log has a hole; serving the batch with `hole` set so the client rebuilds"
        );
    }

    Ok(ConversationState {
        conversation_id: q.conversation_id.clone(),
        authorized: true,
        group_info,
        welcome_pending,
        head,
        head_generation,
        generation,
        commits,
        hole,
        pruned_below,
        commit_at_epoch,
        is_member: Some(true),
        device_registered,
        // Filled by the caller from the main DB.
        device_registered_at: None,
        member_since: None,
        added_identities: Vec::new(),
    })
}

/// The published GroupInfo's `(generation, epoch)`, plus the (possibly large)
/// blob only when asked.
async fn read_group_info(
    tx: &Connection,
    conversation_id: &str,
    want_blob: bool,
) -> Result<Option<GroupInfoState>, AppError> {
    let sql = if want_blob {
        "SELECT generation, epoch, group_info FROM mls_group_info WHERE conversation_id = ?1"
    } else {
        "SELECT generation, epoch, NULL FROM mls_group_info WHERE conversation_id = ?1"
    };
    let mut rows = tx
        .query(sql, libsql::params![conversation_id.to_string()])
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let blob: Option<Vec<u8>> = if want_blob { row.get(2)? } else { None };
    Ok(Some(GroupInfoState {
        generation: row.get(0)?,
        epoch: row.get(1)?,
        group_info: blob.as_deref().map(b64),
    }))
}

/// Whether an undelivered Welcome for this user+device targets the conversation.
///
/// Must match [`pending_welcomes`]' selection, or the gate suppresses an
/// external join for a Welcome the drain never hands back.
async fn welcome_pending(
    tx: &Connection,
    conversation_id: &str,
    user_id: &str,
    device_id: &str,
) -> Result<bool, AppError> {
    let mut rows = tx
        .query(
            "SELECT 1 FROM mls_welcome \
             WHERE conversation_id = ?1 AND recipient_id = ?2 AND delivered = 0 \
             AND (recipient_device_id = ?3 OR recipient_device_id IS NULL) \
             LIMIT 1",
            libsql::params![
                conversation_id.to_string(),
                user_id.to_string(),
                device_id.to_string()
            ],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// The lowest retained epoch in a lineage (`pruned_below`): read in the batch's
/// transaction, it tells a retention prune (#539) from a torn read.
async fn min_epoch_in(
    tx: &Connection,
    conversation_id: &str,
    generation: i64,
) -> Result<Option<i64>, AppError> {
    let mut rows = tx
        .query(
            "SELECT MIN(epoch) FROM mls_commit_log WHERE conversation_id = ?1 AND generation = ?2",
            libsql::params![conversation_id.to_string(), generation],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<Option<i64>>(0)?),
        None => Ok(None),
    }
}

/// The canonical commit bytes at one epoch: an input to the client's
/// Kani-proved `invariants::resolve`, never a verdict.
async fn commit_at(
    tx: &Connection,
    conversation_id: &str,
    generation: i64,
    epoch: i64,
) -> Result<Option<Vec<u8>>, AppError> {
    let mut rows = tx
        .query(
            "SELECT commit_data FROM mls_commit_log \
             WHERE conversation_id = ?1 AND generation = ?2 AND epoch = ?3",
            libsql::params![conversation_id.to_string(), generation, epoch],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row.get::<Vec<u8>>(0)?)),
        None => Ok(None),
    }
}

/// Cross-signing inputs (`account_id_pub` and the added devices' rows) for
/// every user a commit batch adds. Inputs only: the DS is not trusted to
/// evaluate `IdentityDirectory::leaf_verdict`.
async fn added_identities(
    main: &Connection,
    commits: &[pollis_api::commit::CommitWire],
) -> Result<Vec<AddedIdentity>, AppError> {
    // Both columns are CSV and a user may recur across commits, so ids
    // accumulate. They are only a prefetch hint: the client verifies the leaves
    // in the commit's own Add proposals.
    let mut wanted: Vec<(String, Vec<String>)> = Vec::new();
    for c in commits {
        let Some(users) = c.added_user_id.as_deref() else {
            continue;
        };
        let ids: Vec<String> = c
            .added_device_ids
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        for user in users.split(',').map(str::trim).filter(|u| !u.is_empty()) {
            match wanted.iter_mut().find(|(u, _)| u == user) {
                Some((_, existing)) => {
                    for id in &ids {
                        if !existing.contains(id) {
                            existing.push(id.clone());
                        }
                    }
                }
                None => wanted.push((user.to_string(), ids.clone())),
            }
        }
    }

    let mut out = Vec::with_capacity(wanted.len());
    for (user_id, device_ids) in wanted {
        let account_id_pub = account_id_pub(main, &user_id).await?.as_deref().map(b64);
        let devices = device_cert_rows(main, &user_id, &device_ids).await?;
        out.push(AddedIdentity {
            user_id,
            account_id_pub,
            devices,
        });
    }
    Ok(out)
}

/// Cross-signing inputs for a whole roster: each user's `account_id_pub` and
/// every device row, revoked included ("revoked" is a verdict the client acts
/// on; "absent" is not).
pub async fn roster_identities(
    main: &Connection,
    user_ids: &[String],
) -> Result<Vec<AddedIdentity>, AppError> {
    let mut out = Vec::with_capacity(user_ids.len());
    for user_id in user_ids {
        let account_id_pub = account_id_pub(main, user_id).await?.as_deref().map(b64);
        let devices = all_device_cert_rows(main, user_id).await?;
        out.push(AddedIdentity {
            user_id: user_id.clone(),
            account_id_pub,
            devices,
        });
    }
    Ok(out)
}

async fn all_device_cert_rows(
    main: &Connection,
    user_id: &str,
) -> anyhow::Result<Vec<DeviceCertRow>> {
    let mut rows = main
        .query(
            "SELECT device_id, device_cert, cert_issued_at, cert_identity_version, \
                    mls_signature_pub, revoked_at, mls_signature_pub_pq \
             FROM user_device WHERE user_id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(DeviceCertRow {
            device_id: row.get(0)?,
            device_cert: row.get::<Option<Vec<u8>>>(1)?.as_deref().map(b64),
            cert_issued_at: row.get::<Option<String>>(2)?,
            cert_identity_version: row.get::<Option<i64>>(3)?,
            mls_signature_pub: row.get::<Option<Vec<u8>>>(4)?.as_deref().map(b64),
            revoked_at: row.get::<Option<String>>(5)?,
            mls_signature_pub_pq: row.get::<Option<Vec<u8>>>(6)?.as_deref().map(b64),
        });
    }
    Ok(out)
}

/// A user's cross-signing root; `None` for no user or a NULL column (both are a
/// retry to the client).
pub async fn account_id_pub(main: &Connection, user_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let mut rows = main
        .query(
            "SELECT account_id_pub FROM users WHERE id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<Option<Vec<u8>>>(0)?),
        None => Ok(None),
    }
}

/// The cross-signing columns of the named devices that exist, for one user.
pub async fn device_cert_rows(
    main: &Connection,
    user_id: &str,
    device_ids: &[String],
) -> anyhow::Result<Vec<DeviceCertRow>> {
    let mut out = Vec::with_capacity(device_ids.len());
    // `?1` is the user id, so the device ids start at `?2`.
    for chunk in device_ids.chunks(BIND_CHUNK) {
        let mut params: Vec<libsql::Value> = Vec::with_capacity(chunk.len() + 1);
        params.push(user_id.into());
        for did in chunk {
            params.push(did.as_str().into());
        }
        let placeholders = placeholders(chunk.len(), 2);
        let mut rows = main
            .query(
                &format!(
                    "SELECT device_id, device_cert, cert_issued_at, cert_identity_version, \
                            mls_signature_pub, revoked_at, mls_signature_pub_pq \
                     FROM user_device WHERE user_id = ?1 AND device_id IN ({placeholders})"
                ),
                params,
            )
            .await?;
        while let Some(row) = rows.next().await? {
            out.push(DeviceCertRow {
                device_id: row.get(0)?,
                device_cert: row.get::<Option<Vec<u8>>>(1)?.as_deref().map(b64),
                cert_issued_at: row.get::<Option<String>>(2)?,
                cert_identity_version: row.get::<Option<i64>>(3)?,
                mls_signature_pub: row.get::<Option<Vec<u8>>>(4)?.as_deref().map(b64),
                revoked_at: row.get::<Option<String>>(5)?,
                mls_signature_pub_pq: row.get::<Option<Vec<u8>>>(6)?.as_deref().map(b64),
            });
        }
    }
    Ok(out)
}

/// Is this device still registered (not revoked)? Reported apart from
/// `is_member` because a wrong answer has opposite consequences in each.
pub async fn device_registered(
    main: &Connection,
    user_id: &str,
    device_id: &str,
) -> anyhow::Result<bool> {
    let mut rows = main
        .query(
            "SELECT 1 FROM user_device \
             WHERE user_id = ?1 AND device_id = ?2 AND revoked_at IS NULL LIMIT 1",
            libsql::params![user_id.to_string(), device_id.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// When the device was registered, revoked rows included (#1571).
pub async fn device_registered_at(
    main: &Connection,
    user_id: &str,
    device_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut rows = main
        .query(
            "SELECT created_at FROM user_device WHERE user_id = ?1 AND device_id = ?2 LIMIT 1",
            libsql::params![user_id.to_string(), device_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get::<Option<String>>(0)?,
        None => None,
    })
}

/// When `user_id` became a member of `conversation_id` (#1571), over the three
/// id shapes [`is_member`] accepts.
pub async fn member_since(
    main: &Connection,
    conversation_id: &str,
    user_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut rows = main
        .query(
            "SELECT added_at FROM dm_channel_member WHERE dm_channel_id = ?1 AND user_id = ?2 \
             UNION ALL \
             SELECT joined_at FROM group_member WHERE group_id = ?1 AND user_id = ?2 \
             UNION ALL \
             SELECT gm.joined_at FROM channels c \
                 JOIN group_member gm ON gm.group_id = c.group_id \
                 WHERE c.id = ?1 AND gm.user_id = ?2 \
             LIMIT 1",
            libsql::params![conversation_id.to_string(), user_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get::<Option<String>>(0)?,
        None => None,
    })
}

/// POST `/v1/welcomes/fetch` — drain this device's undelivered Welcomes, scoped
/// to the verified device (see [`Reader`]).
pub async fn fetch_welcomes(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: FetchWelcomesBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who =
        match reader(&state, &req, parsed.user_id.as_deref(), parsed.device_id.as_deref()).await? {
            Ok(r) => r,
            Err(resp) => return Ok(resp),
        };
    // A body naming a different recipient is a 403, not an ignored field.
    if let Some(u) = parsed.user_id.as_deref() {
        if state.require_auth && u != who.user_id {
            return Ok(AuthRejection::Forbidden.into_response());
        }
    }

    let conn = state.log_db.conn().await?;
    let welcomes = pending_welcomes(&conn, &who.user_id, &who.device_id).await?;

    // The device's pool size (#1412) and last-resort presence (#1571), when
    // asked. Best-effort: the drain must never fail on this.
    let (unclaimed_key_packages, has_last_resort_key_package) =
        match parsed.count_key_packages_in_suite {
            Some(suite) => {
                let counted = async {
                    let main = state.db.conn().await?;
                    let n = crate::account_reads::unclaimed_key_package_count(
                        &main,
                        &who.user_id,
                        &who.device_id,
                        suite,
                    )
                    .await?;
                    let lr = crate::account_reads::has_last_resort_key_package(
                        &main,
                        &who.user_id,
                        &who.device_id,
                        suite,
                    )
                    .await?;
                    anyhow::Ok((n, lr))
                }
                .await;
                match counted {
                    Ok((n, lr)) => (Some(n), Some(lr)),
                    Err(e) => {
                        tracing::warn!("fetch_welcomes: key-package count failed: {e}");
                        (None, None)
                    }
                }
            }
            None => (None, None),
        };

    Ok(ok_response::<FetchWelcomesBody>(FetchWelcomesResponse {
        welcomes,
        unclaimed_key_packages,
        has_last_resort_key_package,
    }))
}

/// The undelivered Welcomes for one `(user, device)`, oldest first, legacy
/// device-agnostic rows included.
pub async fn pending_welcomes(
    log: &Connection,
    user_id: &str,
    device_id: &str,
) -> anyhow::Result<Vec<PendingWelcome>> {
    // `conversation_id` lets the client check the Welcome's `GroupId` (C1).
    let mut rows = log
        .query(
            "SELECT id, welcome_data, conversation_id FROM mls_welcome \
             WHERE recipient_id = ?1 AND delivered = 0 \
             AND (recipient_device_id = ?2 OR recipient_device_id IS NULL) \
             ORDER BY created_at ASC",
            libsql::params![user_id.to_string(), device_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let bytes: Vec<u8> = row.get(1)?;
        out.push(PendingWelcome {
            id: row.get(0)?,
            welcome: b64(&bytes),
            conversation_id: Some(row.get(2)?),
        });
    }
    Ok(out)
}

/// The commit-batch cross-signing prefetch (#1080).
#[cfg(test)]
mod added_identities_tests {
    use super::*;

    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        // Match production Turso (foreign keys off); local libsql defaults on.
        conn.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&conn).await.expect("schema");
        conn.execute_batch(
            "INSERT INTO users (id, email, username, account_id_pub)
                 VALUES ('alice', 'a@x.com', 'alice', x'0102');
             INSERT INTO users (id, email, username, account_id_pub)
                 VALUES ('bob', 'b@x.com', 'bob', x'0304');
             INSERT INTO user_device (user_id, device_id) VALUES ('alice', 'a2');
             INSERT INTO user_device (user_id, device_id) VALUES ('bob', 'b1');",
        )
        .await
        .unwrap();
        conn
    }

    fn commit(added_user_id: &str, added_device_ids: &str) -> pollis_api::commit::CommitWire {
        pollis_api::commit::CommitWire {
            generation: 0,
            epoch: 1,
            seq: 1,
            sender_id: "alice".to_string(),
            commit: String::new(),
            added_user_id: Some(added_user_id.to_string()),
            added_device_ids: Some(added_device_ids.to_string()),
            created_at: "2026-09-01T00:00:00+00:00".to_string(),
        }
    }

    /// One commit adding devices of two users must prefetch both (#1080).
    #[tokio::test]
    async fn a_multi_user_add_prefetches_every_named_user() {
        let c = conn().await;
        let out = added_identities(&c, &[commit("alice,bob", "a2,b1")])
            .await
            .expect("prefetch");

        let mut users: Vec<&str> = out.iter().map(|i| i.user_id.as_str()).collect();
        users.sort();
        assert_eq!(users, vec!["alice", "bob"]);

        // The device-id hint is shared across users; each gets only their own.
        for identity in &out {
            assert!(
                identity.account_id_pub.is_some(),
                "{} must carry a root key to verify against",
                identity.user_id
            );
            assert_eq!(identity.devices.len(), 1, "{}", identity.user_id);
        }
        let alice = out.iter().find(|i| i.user_id == "alice").unwrap();
        assert_eq!(alice.devices[0].device_id, "a2");
        let bob = out.iter().find(|i| i.user_id == "bob").unwrap();
        assert_eq!(bob.devices[0].device_id, "b1");
    }

    #[tokio::test]
    async fn a_single_user_add_is_unchanged() {
        let c = conn().await;
        let out = added_identities(&c, &[commit("alice", "a2")]).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].user_id, "alice");
        assert_eq!(out[0].devices.len(), 1);
    }

    #[tokio::test]
    async fn a_user_named_twice_accumulates_devices() {
        let c = conn().await;
        let out = added_identities(&c, &[commit("alice", "a2"), commit("alice,bob", "b1")])
            .await
            .unwrap();
        let alice = out.iter().find(|i| i.user_id == "alice").unwrap();
        assert_eq!(
            alice.devices.len(),
            1,
            "alice has one registered device; both ids were asked for"
        );
        assert!(out.iter().any(|i| i.user_id == "bob"));
    }

    #[tokio::test]
    async fn an_absent_hint_prefetches_nothing() {
        let c = conn().await;
        let mut bare = commit("alice", "a2");
        bare.added_user_id = None;
        assert!(added_identities(&c, &[bare]).await.unwrap().is_empty());

        let mut empty = commit("", "");
        empty.added_user_id = Some(String::new());
        assert!(added_identities(&c, &[empty]).await.unwrap().is_empty());
    }
}
