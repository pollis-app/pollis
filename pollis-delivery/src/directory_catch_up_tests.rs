//! #2181 folds catch-up's reads into three round trips. Its answers must be
//! byte for byte what the reads it replaced gave, for every shape: a group
//! with a channel that has no watermark row and one whose `last_seq` is
//! NULL, more conversations than one envelope read binds, a DM, a
//! non-member, an unknown id. `legacy` below is the code as it was before,
//! its SQL verbatim.

use super::*;
use std::sync::Arc;

mod legacy {
    use super::super::*;

    pub(super) async fn resolve_conversation(
        conn: &Connection,
        id: &str,
    ) -> anyhow::Result<Option<(String, ConversationKind)>> {
        let mut rows = conn
            .query(
                "SELECT (SELECT group_id FROM channels WHERE id = ?1), \
                        EXISTS (SELECT 1 FROM dm_channel WHERE id = ?1), \
                        EXISTS (SELECT 1 FROM groups WHERE id = ?1)",
                libsql::params![id.to_string()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        if let Some(group_id) = row.get::<Option<String>>(0)? {
            return Ok(Some((group_id, ConversationKind::Channel)));
        }
        if row.get::<i64>(1)? != 0 {
            return Ok(Some((id.to_string(), ConversationKind::Dm)));
        }
        if row.get::<i64>(2)? != 0 {
            return Ok(Some((id.to_string(), ConversationKind::Group)));
        }
        Ok(None)
    }

    async fn is_member(conn: &Connection, conversation_id: &str, user_id: &str) -> anyhow::Result<bool> {
        let mut rows = conn
            .query(
                "SELECT 1 WHERE \
                    EXISTS (SELECT 1 FROM dm_channel_member \
                            WHERE dm_channel_id = ?1 AND user_id = ?2) \
                 OR EXISTS (SELECT 1 FROM group_member \
                            WHERE group_id = ?1 AND user_id = ?2) \
                 OR EXISTS (SELECT 1 FROM channels c \
                            JOIN group_member gm ON gm.group_id = c.group_id \
                            WHERE c.id = ?1 AND gm.user_id = ?2) \
                 LIMIT 1",
                libsql::params![conversation_id.to_string(), user_id.to_string()],
            )
            .await?;
        Ok(rows.next().await?.is_some())
    }

    async fn fetch_watermarks(
        conn: &Connection,
        conversation_ids: &[String],
        user_id: &str,
        device_id: &str,
    ) -> anyhow::Result<HashMap<String, i64>> {
        let sql = format!(
            "SELECT conversation_id, COALESCE(last_seq, 0) FROM conversation_watermark \
             WHERE user_id = ?1 AND device_id = ?2 AND conversation_id IN ({})",
            crate::util::placeholders(conversation_ids.len(), 3)
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

    async fn fetch_envelopes(
        conn: &Connection,
        conversation_ids: &[String],
        user_id: &str,
        device_id: &str,
    ) -> anyhow::Result<Vec<EnvelopeWire>> {
        let mut out = Vec::new();
        for chunk in conversation_ids.chunks(BIND_CHUNK / 2) {
            let watermarks = fetch_watermarks(conn, chunk, user_id, device_id).await?;
            let mut clauses = Vec::with_capacity(chunk.len());
            let mut params: Vec<libsql::Value> = Vec::with_capacity(chunk.len() * 2);
            for (i, cid) in chunk.iter().enumerate() {
                clauses.push(format!(
                    "(conversation_id = ?{} AND seq > ?{})",
                    i * 2 + 1,
                    i * 2 + 2
                ));
                params.push(cid.clone().into());
                params.push(watermarks.get(cid).copied().unwrap_or(0).into());
            }
            let sql = format!(
                "SELECT conversation_id, id, sender_id, ciphertext, reply_to_id, \
                        target_message_id, sent_at, type, seq, rowid \
                 FROM message_envelope \
                 WHERE {} \
                 ORDER BY conversation_id ASC, seq ASC",
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
                    seq: row.get::<Option<i64>>(8).ok().flatten(),
                    arrival: row.get::<Option<i64>>(9).ok().flatten(),
                });
            }
        }
        Ok(out)
    }

    pub(super) async fn catch_up_answer(
        state: &AppState,
        who: &str,
        device_id: &str,
        parsed: &CatchUpBody,
    ) -> anyhow::Result<CatchUpResponse> {
        let who = who.to_string();
        let device_id = device_id.to_string();
        let conn = state.db.conn().await?;
        let Some((mls_group_id, kind)) = resolve_conversation(&conn, &parsed.conversation_id).await?
        else {
            return Ok(CatchUpResponse {
                authorized: false,
                mls_group_id: parsed.conversation_id.clone(),
                kind: ConversationKind::Group,
                conversation_ids: Vec::new(),
                envelopes: Vec::new(),
                dm: None,
                roster: Vec::new(),
                head: None,
                membership_version: None,
            });
        };
        if !is_member(&conn, &mls_group_id, &who).await? {
            return Ok(CatchUpResponse {
                authorized: false,
                mls_group_id,
                kind,
                conversation_ids: Vec::new(),
                envelopes: Vec::new(),
                dm: None,
                roster: Vec::new(),
                head: None,
                membership_version: None,
            });
        }
        let conversation_ids = match kind {
            ConversationKind::Dm => vec![mls_group_id.clone()],
            _ => crate::util::query_strings(
                &conn,
                "SELECT id FROM channels WHERE group_id = ?1",
                libsql::params![mls_group_id.clone()],
            )
            .await?,
        };
        let (head, envelopes) = if parsed.want_envelopes {
            let log = state.log_db.conn().await?;
            let (generation, epoch) = crate::commit::head(&log, &mls_group_id).await?;
            let head = LogHead { generation, epoch };
            let envelopes = fetch_envelopes(&conn, &conversation_ids, &who, &device_id).await?;
            (Some(head), envelopes)
        } else {
            (None, Vec::new())
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
        let membership_version = if parsed.want_membership_version {
            Some(membership_version(&conn, &mls_group_id).await?)
        } else {
            None
        };
        Ok(CatchUpResponse {
            authorized: true,
            mls_group_id,
            kind,
            conversation_ids,
            envelopes,
            dm,
            roster,
            head,
            membership_version,
        })
    }
}

async fn state() -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::Db::connect_local(dir.path().join("c.db").to_str().unwrap())
        .await
        .unwrap();
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.unwrap();
    let mut sql = String::from(
        "INSERT INTO conversation (id, kind) VALUES ('g', 'group');
         INSERT INTO groups (id, name, owner_id) VALUES ('g', 'G', 'alice');
         INSERT INTO group_member (group_id, user_id, role) VALUES ('g', 'alice', 'admin');
         INSERT INTO group_member (group_id, user_id, role) VALUES ('g', 'bob', 'member');
         INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('i1', 'g', 'alice', 'carol');
         INSERT INTO conversation_membership_version (conversation_id, version) VALUES ('g', 3);
         INSERT INTO conversation (id, kind) VALUES ('d', 'dm');
         INSERT INTO dm_channel (id, created_by) VALUES ('d', 'alice');
         INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, accepted_at) VALUES ('d', 'alice', 'alice', '2026-01-01');
         INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('d', 'bob', 'alice');
         INSERT INTO conversation (id, kind) VALUES ('big', 'group');
         INSERT INTO groups (id, name, owner_id) VALUES ('big', 'Big', 'alice');
         INSERT INTO group_member (group_id, user_id, role) VALUES ('big', 'alice', 'admin');",
    );
    // Channels named out of sorted order, so the list's order is the table's.
    for ch in ["c3", "c1", "c2"] {
        sql.push_str(&format!(
            "INSERT INTO conversation (id, kind) VALUES ('{ch}', 'channel');
             INSERT INTO channels (id, group_id, name) VALUES ('{ch}', 'g', '{ch}');"
        ));
    }
    for i in 0..(BIND_CHUNK / 2 + 13) {
        sql.push_str(&format!(
            "INSERT INTO conversation (id, kind) VALUES ('big{i:03}', 'channel');
             INSERT INTO channels (id, group_id, name) VALUES ('big{i:03}', 'big', 'b{i}');"
        ));
    }
    conn.execute_batch(&sql).await.unwrap();
    for (conv, n) in [("c1", 3), ("c2", 2), ("c3", 2), ("d", 2)] {
        for k in 0..n {
            send(&conn, &format!("{conv}-m{k}"), conv).await;
        }
    }
    for i in 0..(BIND_CHUNK / 2 + 13) {
        send(&conn, &format!("big{i:03}-m"), &format!("big{i:03}")).await;
    }
    // a1's cursors: c1 at 1 (one read, two to come), c2 a row with NULL
    // last_seq, c3 no row at all; d at 1; every other `big` channel read.
    // The cursor column in its one format (#908); catch-up reads `last_seq`.
    let at = crate::messages::seeded_watermark_cursor();
    let mut marks = format!(
        "INSERT INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at, last_seq) VALUES ('c1', 'alice', 'a1', '{at}', 1);
         INSERT INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at, last_seq) VALUES ('c2', 'alice', 'a1', '{at}', NULL);
         INSERT INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at, last_seq) VALUES ('d', 'alice', 'a1', '{at}', 1);",
    );
    for i in (0..(BIND_CHUNK / 2 + 13)).step_by(2) {
        marks.push_str(&format!(
            "INSERT INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at, last_seq) VALUES ('big{i:03}', 'alice', 'a1', '{at}', 1);"
        ));
    }
    conn.execute_batch(&marks).await.unwrap();
    drop(conn);
    (dir, AppState::new(Arc::new(db), false))
}

async fn send(conn: &Connection, id: &str, conversation_id: &str) {
    let body = pollis_api::messages::SendMessageBody {
        id: id.to_string(),
        conversation_id: conversation_id.to_string(),
        sender_id: Some("sealed".into()),
        ciphertext: "mls:00".into(),
        reply_to_id: None,
        sent_at: "2026-09-01T00:00:00+00:00".into(),
        sealed: 1,
        generation: None,
        epoch: None,
        push_to: None,
        push_mentions: None,
        delete_token_hash: Some(crate::messages::FIXTURE_CAPABILITY_HASH.into()),
    };
    let out = crate::messages::apply_send_message(conn, None, &body).await.unwrap();
    assert!(matches!(out, crate::writes::WriteOutcome::Ok), "{out:?}");
}

fn body(conversation_id: &str, flags: [bool; 4]) -> CatchUpBody {
    let [want_envelopes, want_dm, want_roster, want_membership_version] = flags;
    serde_json::from_value(serde_json::json!({
        "conversation_id": conversation_id,
        "want_envelopes": want_envelopes,
        "want_dm": want_dm,
        "want_roster": want_roster,
        "want_membership_version": want_membership_version,
    }))
    .unwrap()
}

#[tokio::test]
async fn the_folded_catch_up_answers_exactly_as_before() {
    let (_dir, state) = state().await;
    let flag_sets = [
        [true, true, true, true],
        [true, false, false, false],
        [false, false, false, true],
        [false, false, false, false],
    ];
    let mut compared = 0;
    for conversation in ["c1", "c2", "c3", "g", "d", "big", "big007", "no-such-id"] {
        for who in ["alice", "bob", "carol", "mallory"] {
            for device in ["a1", "a2"] {
                for flags in flag_sets {
                    let parsed = body(conversation, flags);
                    let new = catch_up_answer(&state, who, device, &parsed).await.unwrap();
                    let old = legacy::catch_up_answer(&state, who, device, &parsed).await.unwrap();
                    assert_eq!(
                        serde_json::to_string(&new).unwrap(),
                        serde_json::to_string(&old).unwrap(),
                        "{conversation} as {who}/{device} with {flags:?}"
                    );
                    compared += 1;
                }
            }
        }
    }
    assert_eq!(compared, 8 * 4 * 2 * 4);
}

/// The shapes the comparison above must have exercised, pinned so a
/// fixture change cannot quietly drop one.
#[tokio::test]
async fn the_fixture_covers_every_watermark_shape() {
    let (_dir, state) = state().await;
    let all = [true, true, true, true];
    let group = catch_up_answer(&state, "alice", "a1", &body("c1", all)).await.unwrap();
    assert!(group.authorized);
    assert_eq!(group.conversation_ids, ["c3", "c1", "c2"], "the table's order");
    let ids: Vec<&str> = group.envelopes.iter().map(|e| e.id.as_str()).collect();
    // c1 past its watermark; c2's NULL last_seq and c3's missing row both read as 0.
    assert_eq!(ids, ["c1-m1", "c1-m2", "c2-m0", "c2-m1", "c3-m0", "c3-m1"]);
    assert_eq!(group.membership_version, Some(3));

    let big = catch_up_answer(&state, "alice", "a1", &body("big", all)).await.unwrap();
    assert!(big.conversation_ids.len() > BIND_CHUNK / 2, "spans two envelope reads");
    assert_eq!(big.envelopes.len(), (BIND_CHUNK / 2 + 13) / 2);

    let fresh_device = catch_up_answer(&state, "alice", "a2", &body("d", all)).await.unwrap();
    assert_eq!(fresh_device.envelopes.len(), 2, "no watermark row at all: everything");

    let outsider = catch_up_answer(&state, "mallory", "m1", &body("c1", all)).await.unwrap();
    assert!(!outsider.authorized);
    assert!(outsider.conversation_ids.is_empty() && outsider.envelopes.is_empty());
    assert!(outsider.head.is_none() && outsider.membership_version.is_none());
    assert_eq!(outsider.mls_group_id, "g");

    let unknown = catch_up_answer(&state, "alice", "a1", &body("no-such-id", all)).await.unwrap();
    assert!(!unknown.authorized);
    assert_eq!(unknown.mls_group_id, "no-such-id");
    assert_eq!(unknown.kind, ConversationKind::Group);
}
