//! Sealing an envelope at the current epoch and posting it under the DS's
//! epoch gate (#1041).
//!
//! An envelope is decryptable only by members still AT the epoch it was sealed
//! at (`max_past_epochs = 0`). The DS refuses to keep one whose asserted
//! `(generation, epoch)` is no longer the commit log's head — a commit landed
//! between this device's catch-up and its post — and answers 409
//! [`pollis_api::messages::EpochBehind`]. The only correct response is the one
//! a lost commit race gets: catch up, re-seal at the new epoch, post again.
//! Every envelope this crate posts — a send, a redaction, an edit — goes
//! through [`post_resealing`], so no post site can forget the retry.

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
    /// Stamped at seal time, and DISPLAY metadata since #1087. A re-seal still
    /// takes a fresh stamp so the bubble shows when the message actually landed,
    /// but nothing depends on it any more: a re-posted envelope takes the next
    /// DS-assigned delivery sequence, so it is above every recipient's cursor
    /// whatever its timestamp says. Before that, refreshing the stamp was
    /// load-bearing — recipients fetched `sent_at > watermark`, so an envelope
    /// re-landing under an old stamp could sit below a cursor that had already
    /// moved past it.
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
/// `Ok(None)` when this device holds no local group for it, or when the group
/// holds a staged, unconfirmed commit — see [`try_seal_unlocked`] and
/// [`seal_under_lock`] for which of those a given caller can actually see.
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
/// The [`MlsLockHeld`] witness is the point (#1079): a post must serialize behind
/// any committer on this device, because a committer holds a staged commit across
/// its DS round-trip and the epoch is undecided for that whole window. Holding
/// the lock means this seal cannot observe that state at all, so `Ok(None)` from
/// the inner sealer can only mean "no local group", which is the error this
/// returns. Requiring proof rather than trusting each site to remember the lock
/// is what keeps a future post site from silently reintroducing the race.
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
/// `Ok(None)` means "not sealable right now" and covers both "no local group"
/// and "a commit is staged" — the caller cannot distinguish them and must not
/// need to. `receipts::emit_receipt` is the only such caller: taking the lock
/// there would deadlock the catch-up paths that hold it while emitting delivered
/// receipts, and a skipped receipt is already its documented failure mode.
pub(super) fn try_seal_unlocked(
    conn: &rusqlite::Connection,
    mls_group_id: &str,
    conversation_id: &str,
    plaintext: &[u8],
) -> Result<Option<Sealed>> {
    try_seal_inner(conn, mls_group_id, conversation_id, plaintext)
}

/// How many times a post re-seals before giving up. Each round is one commit
/// that landed between catch-up and post; a link busy enough to lose four in a
/// row is one on which the user's send should fail visibly rather than spin.
const MAX_RESEALS: usize = 4;

/// What a re-seal needs to know about the envelope being posted.
pub(super) struct Target<'a> {
    pub mls_group_id: &'a str,
    pub conversation_id: &'a str,
    /// The acting user — whose catch-up runs when a re-seal is needed.
    pub user_id: &'a str,
    pub plaintext: &'a [u8],
}

/// Post an envelope, re-sealing and re-posting while the DS reports its epoch
/// behind (#1041).
///
/// `build` turns the current [`Sealed`] into the request body (the caller owns
/// the body type and the other fields). `on_reseal` runs under the local-DB
/// lock after every re-seal, with the connection and the fresh envelope, so a
/// local row written for the first seal can be brought in line (a send's own
/// `message` row, whose `ciphertext` and `sent_at` must match what lands).
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
    } = target;
    // Test-harness only: a test can park a sender here and land a commit
    // while its envelope is sealed-but-unposted (#1041).
    #[cfg(feature = "test-harness")]
    crate::commands::mls::rendezvous::park(
        crate::commands::mls::rendezvous::Point::EnvelopeBeforePost,
    )
    .await;

    for attempt in 1..=MAX_RESEALS {
        match crate::commands::mls::ds_post_envelope(state, &build(&sealed)).await? {
            crate::commands::mls::EnvelopePost::Landed => return Ok(sealed),
            crate::commands::mls::EnvelopePost::EpochBehind {
                head_generation,
                head_epoch,
            } => {
                eprintln!(
                    "[messages] {} for {conversation_id} sealed at ({}, {}) but the log head is \
                     ({head_generation}, {head_epoch}) — catching up and re-sealing (attempt {attempt})",
                    B::PATH,
                    sealed.generation,
                    sealed.epoch
                );
                if attempt == MAX_RESEALS {
                    break;
                }
                // The interleaved catch-up applies the commit(s) we are behind
                // by, decrypting what was sealed at each epoch on the way. It
                // takes the group lock itself; no post site holds it.
                if let Err(e) =
                    super::catch_up_mls_group_interleaved(state, mls_group_id, user_id).await
                {
                    eprintln!(
                        "[messages] re-seal catch-up for {mls_group_id} failed: {e}"
                    );
                }
                // The re-seal needs the MLS lock for the same reason the first
                // seal did (#1079) — and only now that the catch-up has
                // released it. Scoped so it is gone before the next post.
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
