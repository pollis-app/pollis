//! The Vault's durable queue (#2100): a Vault save gets the message outbox's guarantees (#1335).
//!
//! The Vault behaves like a chat with yourself; only its encryption differs (an entry is sealed
//! under the account key, not an MLS group). So a save is queued the way a message is:
//! - **Stored first.** [`enqueue`] writes the `vault_outbox` row, and each file's bytes into
//!   `outbox_attachment` (streamed, never whole in memory), before any network work.
//! - **One id, one entry.** The id is the client's from the first attempt; a repeat with it
//!   adopts the queued row, or answers the saved entry, and the DS stores an entry by id, so a
//!   double submit or a retry after an unanswered save never makes two (#2099).
//! - **Finished later.** A save that fails transiently is retried in place after the outbox's
//!   delays, then left `failed`; the outbox drain ([`drain`], from `flush_outbox`) and Retry
//!   finish it. A process stopped mid-save (the iOS share extension dismissed, the app
//!   suspended) leaves the row for the app.
//! - Files are uploaded under the Vault's file key, so the media view reads them as before.

use std::sync::Arc;

use rusqlite::OptionalExtension;

use super::outbox::{self, AfterFailure, OutboxLock, OutgoingAttachment};
use crate::commands::vault::{self, VaultMessage};
use crate::error::{Error, Result};
use crate::state::AppState;

/// The id a share uses for the Vault among its targets (`share::targets`).
pub const VAULT_TARGET: &str = "vault";

/// What [`enqueue`] found.
pub enum Enqueued {
    /// Queued and not yet saved (now, or by an earlier call with this id): deliver it.
    Unsent(String),
    /// The entry with this id is already in the Vault.
    Saved(VaultMessage),
}

fn queued_row_on(conn: &rusqlite::Connection, id: &str) -> Result<Option<(String, String, String)>> {
    Ok(conn
        .query_row(
            "SELECT content, created_at, send_state FROM vault_outbox WHERE id = ?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?)
}

/// Every queued save, as the Vault screen shows it (with its `send_state`).
pub(crate) fn queued_on(conn: &rusqlite::Connection) -> Result<Vec<VaultMessage>> {
    let mut stmt = conn.prepare(
        "SELECT id, content, created_at, send_state FROM vault_outbox ORDER BY created_at, id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let created_at: String = r.get(2)?;
            Ok(VaultMessage {
                id: r.get(0)?,
                content: r.get(1)?,
                pinned: false,
                updated_at: created_at.clone(),
                created_at,
                send_state: Some(r.get(3)?),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Queue a save: files first, then the row (the commit point), as the outbox does. The id is
/// claimed from the first byte to the row, so another process's orphan sweep leaves the bytes.
pub async fn enqueue(
    state: &Arc<AppState>,
    message_id: Option<String>,
    text: String,
    attachments: Vec<OutgoingAttachment>,
) -> Result<Enqueued> {
    let id = match message_id {
        Some(id) => outbox::checked_message_id(id)?,
        None => outbox::new_message_id(),
    };
    let _sending = crate::notify::lock::suspend::send_started(Some(&id));
    let held = outbox::claim::Claim::take(state, &id).await;
    let result = enqueue_claimed(state, id, text, attachments).await;
    if let Some(held) = held {
        held.release(state).await;
    }
    result
}

async fn enqueue_claimed(
    state: &Arc<AppState>,
    id: String,
    text: String,
    attachments: Vec<OutgoingAttachment>,
) -> Result<Enqueued> {
    {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(outbox::not_signed_in)?;
        if let Some(saved) = vault::get_local(db.conn(), &id)? {
            return Ok(Enqueued::Saved(saved));
        }
        if queued_row_on(db.conn(), &id)?.is_some() {
            return Ok(Enqueued::Unsent(id));
        }
    }
    let metas = outbox::plan_attachments(&attachments).await?;
    outbox::store_all_attachments(state, &id, &attachments, &metas).await?;
    let content = if metas.is_empty() {
        text
    } else {
        outbox::provisional_content(&text, &metas)
    };
    {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(outbox::not_signed_in)?;
        db.conn().execute(
            "INSERT OR IGNORE INTO vault_outbox (id, content, created_at, send_state) VALUES (?1, ?2, ?3, 'pending')",
            rusqlite::params![id, content, super::envelope_sent_at()],
        )?;
    }
    // The bytes are in the database now; staged copies have done their job.
    for staged in attachments.iter().filter_map(|a| a.staged_id.as_deref()) {
        crate::commands::staging::discard_staged(staged);
    }
    Ok(Enqueued::Unsent(id))
}

async fn set_state(state: &Arc<AppState>, id: &str, send_state: &str) {
    let guard = state.local_db.lock().await;
    if let Some(db) = guard.as_ref() {
        if let Err(e) = db.conn().execute(
            "UPDATE vault_outbox SET send_state = ?1 WHERE id = ?2",
            rusqlite::params![send_state, id],
        ) {
            crate::diag!("[vault] could not mark {id} {send_state}: {e}");
        }
    }
}

/// One attempt: upload the files (if any are still stored), save the entry, cache it, and take
/// it out of the queue. Each step is safe to repeat.
async fn attempt(state: &Arc<AppState>, user_id: &str, id: &str) -> Result<VaultMessage> {
    // The row and its stored files in one read, so both come from the same side of a finished
    // upload's transaction (which rewrites the content and deletes the files together).
    let (content, created_at, metas) = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(outbox::not_signed_in)?;
        match queued_row_on(db.conn(), id)? {
            Some((content, created_at, _)) => (content, created_at, outbox::attachment_metas_on(db.conn(), id)?),
            None => {
                return vault::get_local(db.conn(), id)?
                    .ok_or_else(|| Error::Other(anyhow::anyhow!("vault entry {id} is not queued")));
            }
        }
    };
    let content = if metas.is_empty() {
        content
    } else {
        let mut uploaded = Vec::with_capacity(metas.len());
        for (n, meta) in metas.iter().enumerate() {
            uploaded.push(
                outbox::upload_stored_attachment(state, id, crate::commands::r2::UploadScope::Vault, n, meta)
                    .await?,
            );
        }
        let content = outbox::final_content(&outbox::caption_of(&content), &metas, &uploaded);
        // The uploaded envelope replaces the stored bytes, so a retry never uploads again.
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(outbox::not_signed_in)?;
        let tx = db.conn().unchecked_transaction()?;
        tx.execute(
            "UPDATE vault_outbox SET content = ?1 WHERE id = ?2",
            rusqlite::params![content, id],
        )?;
        tx.execute(
            "DELETE FROM outbox_attachment WHERE message_id = ?1",
            rusqlite::params![id],
        )?;
        tx.commit()?;
        content
    };
    vault::save_entry(state, user_id, id, &vault::VaultEntryPlain::new(content.clone()), &created_at).await?;
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(outbox::not_signed_in)?;
    let tx = db.conn().unchecked_transaction()?;
    vault::upsert_local(&tx, id, &content, false, &created_at, &created_at)?;
    tx.execute("DELETE FROM vault_outbox WHERE id = ?1", rusqlite::params![id])?;
    tx.commit()?;
    Ok(VaultMessage {
        id: id.to_string(),
        content,
        pinned: false,
        created_at: created_at.clone(),
        updated_at: created_at,
        send_state: None,
    })
}

/// Save a queued entry, retrying a transient failure in place after the outbox's delays; past
/// them it reads `failed` and waits for the drain or a Retry. A refusal no retry fixes is
/// `rejected` at once.
pub async fn deliver(state: &Arc<AppState>, user_id: &str, id: &str) -> Result<VaultMessage> {
    let _sending = crate::notify::lock::suspend::send_started(Some(id));
    let lock = OutboxLock::acquire(state, id).await;
    set_state(state, id, "pending").await;
    let mut delays = outbox::retry_delays().into_iter();
    let result = loop {
        let err = match attempt(state, user_id, id).await {
            Ok(entry) => break Ok(entry),
            Err(e) => e,
        };
        let decision = outbox::after_failure(
            &err,
            false,
            crate::notify::lock::suspend::is_leaving(),
            outbox::known_offline().await,
        );
        let delay = delays.next();
        match (decision, delay) {
            (AfterFailure::Retry, Some(delay)) => {
                crate::diag!("[vault] save of {id} failed ({err}); retrying");
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = crate::notify::lock::suspend::left() => {}
                }
            }
            (AfterFailure::Stop, _) if outbox::is_permanent(&err) => {
                set_state(state, id, "rejected").await;
                break Err(err);
            }
            _ => {
                set_state(state, id, "failed").await;
                break Err(err);
            }
        }
    };
    lock.release().await;
    result
}

/// One attempt at every queued save that may still go (`pending` or `failed`), for the outbox
/// drain. Returns `(saved, failed)` ids.
pub(crate) async fn drain(state: &Arc<AppState>) -> Result<(Vec<String>, Vec<String>)> {
    let ids: Vec<String> = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else {
            return Ok((Vec::new(), Vec::new()));
        };
        super::strings_on(
            db.conn(),
            "SELECT id FROM vault_outbox WHERE send_state IN ('pending', 'failed') ORDER BY created_at, id",
        )?
    };
    if ids.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let user_id = crate::commands::mls::current_user_id(state).await?;
    let (mut saved, mut failed) = (Vec::new(), Vec::new());
    for id in ids {
        // Another process (the share extension) is saving it.
        if outbox::claim::held_elsewhere(state, &id).await {
            outbox::claim::redrain_after_lease(state);
            continue;
        }
        let _sending = crate::notify::lock::suspend::send_started(Some(&id));
        let lock = OutboxLock::acquire(state, &id).await;
        match attempt(state, &user_id, &id).await {
            Ok(_) => saved.push(id.clone()),
            Err(e) => {
                crate::diag!("[vault] queued save of {id} failed: {e}");
                let permanent = outbox::is_permanent(&e);
                set_state(state, &id, if permanent { "rejected" } else { "failed" }).await;
                failed.push(id.clone());
            }
        }
        lock.release().await;
    }
    Ok((saved, failed))
}

/// Whether the queued save `id` was refused for good.
pub(crate) async fn is_rejected(state: &Arc<AppState>, id: &str) -> bool {
    let guard = state.local_db.lock().await;
    guard
        .as_ref()
        .and_then(|db| queued_row_on(db.conn(), id).ok().flatten())
        .is_some_and(|(_, _, s)| s == "rejected")
}

/// The Vault screen's Retry on a save that reads "Not sent".
pub async fn retry_vault_message(id: String, user_id: String, state: &Arc<AppState>) -> Result<VaultMessage> {
    deliver(state, &user_id, &id).await
}

/// Drop a queued save that was never saved, with its files. False when there is none.
pub async fn discard_unsent_vault_message(id: String, state: &Arc<AppState>) -> Result<bool> {
    let lock = OutboxLock::acquire(state, &id).await;
    let removed = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(outbox::not_signed_in)?;
        let tx = db.conn().unchecked_transaction()?;
        tx.execute("DELETE FROM outbox_attachment WHERE message_id = ?1", rusqlite::params![id])?;
        let n = tx.execute("DELETE FROM vault_outbox WHERE id = ?1", rusqlite::params![id])?;
        tx.commit()?;
        n > 0
    };
    lock.release().await;
    Ok(removed)
}
