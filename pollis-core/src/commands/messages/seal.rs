//! Sealing an envelope at the current epoch and posting it under the DS's
//! epoch gate (#1041).
//!
//! With `max_past_epochs = 0` an envelope is readable only at the epoch it was
//! sealed at, so the DS refuses one whose `(generation, epoch)` is no longer the
//! log head (409 [`pollis_api::messages::EpochBehind`]). The fix is to catch up,
//! re-seal and post again; every envelope goes through [`post_resealing`], so no
//! post site can forget it.

use std::sync::Arc;

use pollis_api::ClientRequest;

use crate::error::Result;
use crate::state::AppState;

/// One application message sealed at a definite `(generation, epoch)`.
pub(super) struct Sealed {
    /// The TLS-serialised `MlsMessageOut`, as the local row stores it.
    pub bytes: Vec<u8>,
    /// Read off the ciphertext's own MLS header, never from the server.
    pub generation: i64,
    pub epoch: i64,
    /// Display metadata only (#1087): a re-posted envelope takes the next DS
    /// sequence, so its stamp affects nothing but the bubble.
    pub sent_at: String,
}

impl Sealed {
    /// The `"mls:<hex>"` form `message_envelope.ciphertext` stores.
    pub fn wire(&self) -> String {
        format!("mls:{}", hex::encode(&self.bytes))
    }
}

/// Encrypt `plaintext` at the epoch the local group sits at now. `Ok(None)`
/// when there is no local group, or a staged commit is pending.
fn try_seal_inner(
    conn: &rusqlite::Connection,
    mls_group_id: &str,
    conversation_id: &str,
    plaintext: &[u8],
) -> Result<Option<Sealed>> {
    let Some(bytes) = crate::commands::mls::try_mls_encrypt(conn, mls_group_id, plaintext) else {
        return Ok(None);
    };
    let (generation, epoch) = crate::commands::mls::envelope_lineage(&bytes).ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!(
            "sealed envelope for {conversation_id} has no readable MLS lineage"
        ))
    })?;
    Ok(Some(Sealed {
        bytes,
        generation,
        epoch: epoch as i64,
        sent_at: super::envelope_sent_at(),
    }))
}

/// Seal for a caller that holds the per-conversation MLS lock, the shape every
/// user-initiated post wants.
///
/// The [`MlsLockHeld`] witness (#1079): a committer holds a staged commit
/// across its DS round trip, so a post must serialize behind it. Under the
/// lock, `Ok(None)` can only mean "no local group". Take the lock before
/// `state.local_db` (as `reconcile` and `self_update` do) and drop it before
/// any `await` that might take it (the catch-up does).
pub(super) fn seal_under_lock(
    conn: &rusqlite::Connection,
    mls_group_id: &str,
    conversation_id: &str,
    plaintext: &[u8],
    _held: &crate::state::MlsLockHeld<'_>,
) -> Result<Sealed> {
    try_seal_inner(conn, mls_group_id, conversation_id, plaintext)?.ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!(
            "MLS group not initialized for conversation {conversation_id}"
        ))
    })
}

/// Seal for a caller that deliberately holds no MLS lock. `Ok(None)` covers
/// both "no local group" and "a commit is staged". Only `receipts::emit_receipt`
/// uses it: taking the lock there would deadlock the catch-up paths that emit
/// delivered receipts, and a skipped receipt is acceptable.
pub(super) fn try_seal_unlocked(
    conn: &rusqlite::Connection,
    mls_group_id: &str,
    conversation_id: &str,
    plaintext: &[u8],
) -> Result<Option<Sealed>> {
    try_seal_inner(conn, mls_group_id, conversation_id, plaintext)
}

/// How many times a post re-seals before giving up. Losing four commit races
/// in a row should fail the send visibly rather than spin.
const MAX_RESEALS: usize = 4;

/// What a re-seal needs to know about the envelope being posted.
pub(super) struct Target<'a> {
    pub mls_group_id: &'a str,
    pub conversation_id: &'a str,
    /// The acting user — whose catch-up runs when a re-seal is needed.
    pub user_id: &'a str,
    pub plaintext: &'a [u8],
    /// Recorded as this device's own before the first post (#1312): it comes
    /// back through the fetch, undecryptable to us.
    pub envelope_id: &'a str,
}

/// Post an envelope, re-sealing while the DS reports its epoch behind (#1041).
///
/// `build` turns the current [`Sealed`] into the request body. `on_reseal` runs
/// under the local-DB lock after every re-seal, so a local row written for the
/// first seal can match what lands. Returns the envelope that landed.
pub(super) async fn post_resealing<B: ClientRequest>(
    state: &Arc<AppState>,
    target: Target<'_>,
    mut sealed: Sealed,
    build: impl Fn(&Sealed) -> B,
    on_reseal: impl Fn(&rusqlite::Connection, &Sealed) -> Result<()>,
) -> Result<Sealed> {
    let Target {
        mls_group_id,
        conversation_id,
        user_id,
        plaintext,
        envelope_id,
    } = target;
    {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            super::loss::record_own_envelope(db.conn(), envelope_id);
        }
    }
    #[cfg(feature = "test-harness")]
    crate::commands::mls::rendezvous::park(
        crate::commands::mls::rendezvous::Point::EnvelopeBeforePost,
    )
    .await;
    // Test-harness only: the process dies with the envelope sealed — the
    // sender's ratchet already past it — and not posted (#1524).
    if super::send::test_hooks::take(super::send::test_hooks::Crash::AfterSeal) {
        return Err(super::send::test_hooks::simulated_crash());
    }

    for attempt in 1..=MAX_RESEALS {
        match crate::commands::mls::ds_post_envelope(state, &build(&sealed)).await? {
            crate::commands::mls::EnvelopePost::Landed => return Ok(sealed),
            crate::commands::mls::EnvelopePost::EpochBehind {
                head_generation,
                head_epoch,
            } => {
                crate::diag!(
                    "[messages] {} for {conversation_id} sealed at ({}, {}) but the log head is \
                     ({head_generation}, {head_epoch}) — catching up and re-sealing (attempt {attempt})",
                    B::PATH,
                    sealed.generation,
                    sealed.epoch
                );
                if attempt == MAX_RESEALS {
                    break;
                }
                // The catch-up takes the group lock itself; no post site holds it.
                if let Err(e) =
                    super::catch_up_mls_group_interleaved(state, mls_group_id, user_id).await
                {
                    crate::diag!(
                        "[messages] re-seal catch-up for {mls_group_id} failed: {e}"
                    );
                }
                // Re-seal under the MLS lock (#1079), now that the catch-up has
                // released it; scoped so it is gone before the next post.
                let mls_guard = state.mls_group_lock(mls_group_id).await;
                let held = crate::state::MlsLockHeld::from_guard(&mls_guard);
                let guard = state.local_db.lock().await;
                let db = guard.as_ref().ok_or_else(|| {
                    crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
                })?;
                sealed =
                    seal_under_lock(db.conn(), mls_group_id, conversation_id, plaintext, &held)?;
                on_reseal(db.conn(), &sealed)?;
            }
        }
    }
    Err(crate::error::Error::Other(anyhow::anyhow!(
        "envelope for {conversation_id} could not be sealed at the log head after \
         {MAX_RESEALS} attempts"
    )))
}
