//! Sealing an envelope at the current epoch and posting it under the DS's
//! epoch gate (#1041).
//!
//! The DS answers 409 `EpochBehind` when a commit landed first; [`post_resealing`] catches up, re-seals, retries.

use std::sync::Arc;

use pollis_api::ClientRequest;

use crate::error::Result;
use crate::state::AppState;

/// One application message sealed at a definite `(generation, epoch)`.
pub(super) struct Sealed {
    /// The TLS-serialised `MlsMessageOut`, exactly what the local `message`
    /// row's `ciphertext` column holds.
    pub bytes: Vec<u8>,
    /// The lineage read off the ciphertext's own MLS header — asserted to the
    /// DS, never taken from anywhere the server could influence.
    pub generation: i64,
    pub epoch: i64,
    /// Stamped at seal time; display-only since #1087.
    pub sent_at: String,
}

impl Sealed {
    /// The `"mls:<hex>"` form `message_envelope.ciphertext` stores.
    pub fn wire(&self) -> String {
        format!("mls:{}", hex::encode(&self.bytes))
    }
}

/// Encrypt `plaintext` for `mls_group_id` at the epoch the local group sits at
/// right now.
///
/// `Ok(None)` when no local group exists or a commit is staged.
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

/// Seal for a caller that holds the per-conversation MLS lock — the shape every
/// user-initiated post wants.
///
/// The [`MlsLockHeld`] witness (#1079) serializes posts behind any committer's staged commit.
///
/// Take the lock BEFORE `state.local_db` — the order `reconcile` and
/// `self_update` use — and drop it before any `await` that might itself take it
/// (the catch-up does).
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

/// Seal for a caller that deliberately holds NO MLS lock, where not sealing is
/// an acceptable outcome.
///
/// `Ok(None)` covers no group and a staged commit. Only for `emit_receipt`, where locking would deadlock catch-up.
pub(super) fn try_seal_unlocked(
    conn: &rusqlite::Connection,
    mls_group_id: &str,
    conversation_id: &str,
    plaintext: &[u8],
) -> Result<Option<Sealed>> {
    try_seal_inner(conn, mls_group_id, conversation_id, plaintext)
}

/// How many times a post re-seals before giving up.
const MAX_RESEALS: usize = 4;

/// What a re-seal needs to know about the envelope being posted.
pub(super) struct Target<'a> {
    pub mls_group_id: &'a str,
    pub conversation_id: &'a str,
    /// The acting user — whose catch-up runs when a re-seal is needed.
    pub user_id: &'a str,
    pub plaintext: &'a [u8],
    /// The envelope's id, recorded as this device's own before the first post
    /// (#1312) — it will come back through the fetch, undecryptable to us.
    pub envelope_id: &'a str,
}

/// Post an envelope, re-sealing and re-posting while the DS reports its epoch
/// behind (#1041).
///
/// `build` makes the body from a [`Sealed`]; `on_reseal` fixes local rows under the DB lock after each re-seal.
///
/// Returns the envelope that landed — its `sent_at` is the stamp the caller
/// must report, not the one it started with.
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
    // Test-harness only: a test can park a sender here and land a commit
    // while its envelope is sealed-but-unposted (#1041).
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
                // The interleaved catch-up takes the group lock itself; no post site holds it.
                if let Err(e) =
                    super::catch_up_mls_group_interleaved(state, mls_group_id, user_id).await
                {
                    crate::diag!(
                        "[messages] re-seal catch-up for {mls_group_id} failed: {e}"
                    );
                }
                // Re-take the MLS lock (#1079) only now the catch-up released it.
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
