//! Channel categories and channel order within a group (#2031).
//!
//! A group's channel list is blocks: the uncategorised block first, then one
//! block per category in category order. Every member sees the same blocks in
//! the same order; only an admin of the group may change them.
//!
//! ## Invariants
//!
//! - **One position per block.** Every change renumbers each block it touches
//!   from 0 inside one write transaction ([`crate::db::write_tx`], `BEGIN
//!   IMMEDIATE`), so two admins dragging at once serialize; migration 000052's
//!   partial unique indexes refuse a duplicate even from a writer that skips
//!   this module.
//! - **A category delete never deletes a channel.** Its channels move to the end
//!   of the uncategorised block in their order before the category row goes, and
//!   migration 000052's trigger detaches any a writer left behind.
//! - **A channel only ever sits in a category of its own group** — checked here
//!   and, again, by migration 000052's triggers.
//!
//! The display order these produce is read back by
//! [`crate::directory::CHANNEL_ORDER_SQL`].

use axum::{extract::State, response::Response};
use libsql::Connection;

use crate::directory::{CATEGORY_JOIN, CHANNEL_ORDER_SQL};
use crate::error::AppError;
use crate::groups::is_admin;
use crate::util::query_strings;
use crate::writes::{gate_and_parse, outcome_response, resolve_actor, RawRequest, WriteOutcome};
use crate::AppState;

pub use pollis_api::groups::{
    CreateChannelCategoryBody,
    DeleteChannelCategoryBody,
    MoveChannelBody,
    MoveChannelCategoryBody,
    RenameChannelCategoryBody,
};

/// True when `category_id` is a category of `group_id`.
pub(crate) async fn category_in_group(
    conn: &Connection,
    category_id: &str,
    group_id: &str,
) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM channel_category WHERE id = ?1 AND group_id = ?2",
            libsql::params![category_id.to_string(), group_id.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// The ids of one block's channels, in display order. `None` is the
/// uncategorised block, which also holds any channel naming a category that is
/// not one of this group's (it renders there, so it is renumbered there).
pub(crate) async fn block_channels(
    conn: &Connection,
    group_id: &str,
    category_id: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    // `?2 IS NULL` selects the uncategorised block, `cc.id = ?2` a category.
    let sql = format!(
        "SELECT c.id FROM channels c \
         LEFT JOIN channel_category cc ON {CATEGORY_JOIN} \
         WHERE c.group_id = ?1 \
           AND ((?2 IS NULL AND cc.id IS NULL) OR cc.id = ?2) \
         ORDER BY {CHANNEL_ORDER_SQL}"
    );
    query_strings(conn, &sql, libsql::params![group_id.to_string(), category_id.map(str::to_string)]).await
}

/// Make `ids` the whole of one block, in this order, positions 0.. — every
/// position cleared first, so no intermediate row can collide with the unique
/// index. `ids` must hold every channel the block should keep.
pub(crate) async fn renumber_block(
    conn: &Connection,
    category_id: Option<&str>,
    ids: &[String],
) -> anyhow::Result<()> {
    for id in ids {
        conn.execute(
            "UPDATE channels SET position = NULL WHERE id = ?1",
            libsql::params![id.clone()],
        )
        .await?;
    }
    for (i, id) in ids.iter().enumerate() {
        conn.execute(
            "UPDATE channels SET category_id = ?1, position = ?2 WHERE id = ?3",
            libsql::params![category_id.map(str::to_string), i as i64, id.clone()],
        )
        .await?;
    }
    Ok(())
}

/// The group's category ids in order.
async fn group_categories(conn: &Connection, group_id: &str) -> anyhow::Result<Vec<String>> {
    query_strings(
        conn,
        "SELECT id FROM channel_category WHERE group_id = ?1 ORDER BY position",
        libsql::params![group_id.to_string()],
    )
    .await
}

/// Make `ids` the group's categories in this order, positions 0... Every
/// position is first lifted past the current maximum, so no intermediate row
/// collides with the unique index (positions are never negative).
async fn renumber_categories(conn: &Connection, group_id: &str, ids: &[String]) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE channel_category \
         SET position = position + (SELECT MAX(position) + 1 FROM channel_category WHERE group_id = ?1) \
         WHERE group_id = ?1",
        libsql::params![group_id.to_string()],
    )
    .await?;
    for (i, id) in ids.iter().enumerate() {
        conn.execute(
            "UPDATE channel_category SET position = ?1 WHERE id = ?2",
            libsql::params![i as i64, id.clone()],
        )
        .await?;
    }
    Ok(())
}

/// `list` with `item` at `position`, clamped to the end.
fn insert_at(mut list: Vec<String>, item: String, position: u32) -> Vec<String> {
    let at = (position as usize).min(list.len());
    list.insert(at, item);
    list
}

/// The actor, when they may arrange `group_id`: a re-derived admin (auth on) or
/// any named actor (auth off, as for every domain-B write).
async fn arranging_admin(
    conn: &Connection,
    authed: Option<&str>,
    requester: Option<&str>,
    group_id: &str,
) -> anyhow::Result<Result<String, WriteOutcome>> {
    let actor = match resolve_actor(authed, requester) {
        Ok(a) => a,
        Err(o) => return Ok(Err(o)),
    };
    if authed.is_some() && !is_admin(conn, group_id, &actor).await? {
        return Ok(Err(WriteOutcome::Forbidden));
    }
    Ok(Ok(actor))
}

/// [`arranging_admin`], then: `category_id` is a category of `group_id`.
async fn admin_of_category(
    conn: &Connection,
    authed: Option<&str>,
    requester: Option<&str>,
    group_id: &str,
    category_id: &str,
) -> anyhow::Result<Option<WriteOutcome>> {
    if let Err(o) = arranging_admin(conn, authed, requester, group_id).await? {
        return Ok(Some(o));
    }
    if !category_in_group(conn, category_id, group_id).await? {
        return Ok(Some(WriteOutcome::Forbidden));
    }
    Ok(None)
}

macro_rules! handler {
    ($name:ident, $body:ty, $apply:ident) => {
        pub async fn $name(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
            let (authed, parsed) = match gate_and_parse::<$body>(&state, &req).await? {
                Ok(v) => v,
                Err(resp) => return Ok(resp),
            };
            let conn = state.db.conn().await?;
            outcome_response::<$body>($apply(&conn, authed.as_deref(), &parsed).await?)
        }
    };
}

handler!(create_category, CreateChannelCategoryBody, apply_create_category);
handler!(rename_category, RenameChannelCategoryBody, apply_rename_category);
handler!(delete_category, DeleteChannelCategoryBody, apply_delete_category);
handler!(move_category, MoveChannelCategoryBody, apply_move_category);
handler!(move_channel, MoveChannelBody, apply_move_channel);

/// Commit on `Ok`, roll back on a refusal.
async fn finish(tx: libsql::Transaction, outcome: WriteOutcome) -> anyhow::Result<WriteOutcome> {
    if matches!(outcome, WriteOutcome::Ok) {
        tx.commit().await?;
    } else {
        tx.rollback().await?;
    }
    Ok(outcome)
}

/// A new category at the bottom of the group's categories. Admin-only; the name
/// follows the group-name rule (`profile::is_valid_display_name`).
pub async fn apply_create_category(
    conn: &Connection,
    authed: Option<&str>,
    body: &CreateChannelCategoryBody,
) -> anyhow::Result<WriteOutcome> {
    if !crate::profile::is_valid_display_name(&body.name) {
        return Ok(WriteOutcome::Invalid(crate::profile::DISPLAY_NAME_INVALID));
    }
    if body.id.is_empty() || body.id.len() > 128 {
        return Ok(WriteOutcome::Invalid("invalid category id"));
    }
    let tx = crate::db::write_tx(conn).await?;
    let outcome = create_category_in(&tx, authed, body).await?;
    finish(tx, outcome).await
}

async fn create_category_in(
    tx: &Connection,
    authed: Option<&str>,
    body: &CreateChannelCategoryBody,
) -> anyhow::Result<WriteOutcome> {
    if let Err(o) = arranging_admin(tx, authed, body.requester_id.as_deref(), &body.group_id).await? {
        return Ok(o);
    }
    if !crate::groups::group_exists(tx, &body.group_id).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    let mut taken = tx
        .query(
            "SELECT 1 FROM channel_category WHERE id = ?1",
            libsql::params![body.id.clone()],
        )
        .await?;
    if taken.next().await?.is_some() {
        return Ok(WriteOutcome::Forbidden);
    }
    drop(taken);
    tx.execute(
        "INSERT INTO channel_category (id, group_id, name, position) \
         VALUES (?1, ?2, ?3, \
                 (SELECT COALESCE(MAX(position) + 1, 0) FROM channel_category WHERE group_id = ?2))",
        libsql::params![body.id.clone(), body.group_id.clone(), body.name.clone()],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

/// Rename a category. Admin-only; same name rule as create.
pub async fn apply_rename_category(
    conn: &Connection,
    authed: Option<&str>,
    body: &RenameChannelCategoryBody,
) -> anyhow::Result<WriteOutcome> {
    if !crate::profile::is_valid_display_name(&body.name) {
        return Ok(WriteOutcome::Invalid(crate::profile::DISPLAY_NAME_INVALID));
    }
    let tx = crate::db::write_tx(conn).await?;
    let outcome = rename_category_in(&tx, authed, body).await?;
    finish(tx, outcome).await
}

async fn rename_category_in(
    tx: &Connection,
    authed: Option<&str>,
    body: &RenameChannelCategoryBody,
) -> anyhow::Result<WriteOutcome> {
    if let Some(refused) =
        admin_of_category(tx, authed, body.requester_id.as_deref(), &body.group_id, &body.category_id).await?
    {
        return Ok(refused);
    }
    tx.execute(
        "UPDATE channel_category SET name = ?1 WHERE id = ?2",
        libsql::params![body.name.clone(), body.category_id.clone()],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

/// Delete a category. Its channels go to the end of the uncategorised block,
/// in their order, after the channels already there; no channel is deleted.
pub async fn apply_delete_category(
    conn: &Connection,
    authed: Option<&str>,
    body: &DeleteChannelCategoryBody,
) -> anyhow::Result<WriteOutcome> {
    let tx = crate::db::write_tx(conn).await?;
    let outcome = delete_category_in(&tx, authed, body).await?;
    finish(tx, outcome).await
}

async fn delete_category_in(
    tx: &Connection,
    authed: Option<&str>,
    body: &DeleteChannelCategoryBody,
) -> anyhow::Result<WriteOutcome> {
    if let Some(refused) =
        admin_of_category(tx, authed, body.requester_id.as_deref(), &body.group_id, &body.category_id).await?
    {
        return Ok(refused);
    }
    let mut top = block_channels(tx, &body.group_id, None).await?;
    top.extend(block_channels(tx, &body.group_id, Some(&body.category_id)).await?);
    renumber_block(tx, None, &top).await?;
    tx.execute(
        "DELETE FROM channel_category WHERE id = ?1",
        libsql::params![body.category_id.clone()],
    )
    .await?;
    let rest = group_categories(tx, &body.group_id).await?;
    renumber_categories(tx, &body.group_id, &rest).await?;
    Ok(WriteOutcome::Ok)
}

/// Put a category at `position` among its group's categories.
pub async fn apply_move_category(
    conn: &Connection,
    authed: Option<&str>,
    body: &MoveChannelCategoryBody,
) -> anyhow::Result<WriteOutcome> {
    let tx = crate::db::write_tx(conn).await?;
    let outcome = move_category_in(&tx, authed, body).await?;
    finish(tx, outcome).await
}

async fn move_category_in(
    tx: &Connection,
    authed: Option<&str>,
    body: &MoveChannelCategoryBody,
) -> anyhow::Result<WriteOutcome> {
    if let Some(refused) =
        admin_of_category(tx, authed, body.requester_id.as_deref(), &body.group_id, &body.category_id).await?
    {
        return Ok(refused);
    }
    let others: Vec<String> = group_categories(tx, &body.group_id)
        .await?
        .into_iter()
        .filter(|id| id != &body.category_id)
        .collect();
    let order = insert_at(others, body.category_id.clone(), body.position);
    renumber_categories(tx, &body.group_id, &order).await?;
    Ok(WriteOutcome::Ok)
}

/// Move a channel to `position` in a block of its own group (a category of
/// that group, or the uncategorised block). Both blocks it touches are
/// renumbered in the same transaction.
pub async fn apply_move_channel(
    conn: &Connection,
    authed: Option<&str>,
    body: &MoveChannelBody,
) -> anyhow::Result<WriteOutcome> {
    let tx = crate::db::write_tx(conn).await?;
    let outcome = move_channel_in(&tx, authed, body).await?;
    finish(tx, outcome).await
}

async fn move_channel_in(
    tx: &Connection,
    authed: Option<&str>,
    body: &MoveChannelBody,
) -> anyhow::Result<WriteOutcome> {
    if let Err(o) = arranging_admin(tx, authed, body.requester_id.as_deref(), &body.group_id).await? {
        return Ok(o);
    }
    // The channel's own block, read in the transaction (the join drops a
    // category that is not this group's, so the channel reads as uncategorised).
    let mut rows = tx
        .query(
            &format!(
                "SELECT cc.id FROM channels c \
                 LEFT JOIN channel_category cc ON {CATEGORY_JOIN} \
                 WHERE c.id = ?1 AND c.group_id = ?2"
            ),
            libsql::params![body.channel_id.clone(), body.group_id.clone()],
        )
        .await?;
    let source: Option<String> = match rows.next().await? {
        Some(row) => row.get(0)?,
        // Not a channel of this group.
        None => return Ok(WriteOutcome::Forbidden),
    };
    drop(rows);
    if let Some(cat) = &body.category_id {
        if !category_in_group(tx, cat, &body.group_id).await? {
            return Ok(WriteOutcome::Forbidden);
        }
    }
    let target = body.category_id.as_deref();
    let others: Vec<String> = block_channels(tx, &body.group_id, target)
        .await?
        .into_iter()
        .filter(|id| id != &body.channel_id)
        .collect();
    let order = insert_at(others, body.channel_id.clone(), body.position);
    // Free the moved channel's old slot before either block is renumbered.
    tx.execute(
        "UPDATE channels SET position = NULL WHERE id = ?1",
        libsql::params![body.channel_id.clone()],
    )
    .await?;
    if source.as_deref() != target {
        let left: Vec<String> = block_channels(tx, &body.group_id, source.as_deref())
            .await?
            .into_iter()
            .filter(|id| id != &body.channel_id)
            .collect();
        renumber_block(tx, source.as_deref(), &left).await?;
    }
    renumber_block(tx, target, &order).await?;
    Ok(WriteOutcome::Ok)
}


#[cfg(test)]
mod tests {
    use super::insert_at;

    fn v(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn insert_at_clamps_past_the_end() {
        assert_eq!(insert_at(v(&["a", "b"]), "x".into(), 0), v(&["x", "a", "b"]));
        assert_eq!(insert_at(v(&["a", "b"]), "x".into(), 1), v(&["a", "x", "b"]));
        assert_eq!(insert_at(v(&["a", "b"]), "x".into(), 9), v(&["a", "b", "x"]));
        assert_eq!(insert_at(Vec::new(), "x".into(), 3), v(&["x"]));
    }
}
