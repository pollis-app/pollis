//! Inbound MLS `Welcome` handling.
//!
//! Welcomes are how new members are added to an existing MLS group. The
//! committer generates a Welcome blob targeting each invitee's KeyPackage;
//! invitees process it locally to materialise the group state.

use openmls::prelude::*;
use openmls_traits::OpenMlsProvider;

use std::sync::Arc;
use tls_codec::Deserialize as TlsDeserialize;

use crate::error::Result;
use crate::state::AppState;

use super::key_packages::replenish_key_packages;
use super::provider::PollisProvider;

/// Internal: deserialise a TLS-encoded `MlsMessageOut` (welcome wire format)
/// and persist the resulting MLS group state locally.
///
/// The bytes stored in `mls_welcome.welcome_data` are TLS-serialised
/// `MlsMessageOut`.  We deserialise to `MlsMessageIn`, extract the inner
/// `Welcome` via `MlsMessageIn::extract()`, then call
/// `StagedWelcome::new_from_welcome`.
pub async fn apply_welcome(state: &Arc<AppState>, welcome_bytes: &[u8]) -> Result<()> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
    })?;
    let provider = PollisProvider::new(db.conn());

    let mut reader: &[u8] = welcome_bytes;
    let msg_in = MlsMessageIn::tls_deserialize(&mut reader)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("welcome msg deserialize: {e}")))?;

    let welcome = match msg_in.extract() {
        MlsMessageBodyIn::Welcome(w) => w,
        _ => return Err(crate::error::Error::Other(anyhow::anyhow!(
            "expected Welcome message in mls_welcome"
        ))),
    };

    let join_config = MlsGroupJoinConfig::builder()
        .use_ratchet_tree_extension(true)
        .build();

    // Split into ProcessedWelcome → delete stale group → stage → into_group.
    // openmls checks for duplicate GroupIds inside `into_staged_welcome`, so we
    // must delete any existing group *before* that call.
    let processed = ProcessedWelcome::new_from_welcome(&provider, &join_config, welcome)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("process welcome: {e}")))?;

    let new_group_id = processed.unverified_group_info().group_id().clone();
    if let Ok(Some(mut old_group)) = MlsGroup::load(provider.storage(), &new_group_id) {
        eprintln!("[mls] apply_welcome: deleting stale group {:?} before re-joining", new_group_id);
        let _ = old_group.delete(provider.storage());
    }

    let staged = processed.into_staged_welcome(&provider, None)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("stage welcome: {e}")))?;

    staged.into_group(&provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("into group: {e}")))?;

    Ok(())
}

/// Process a TLS-encoded MLS `Welcome` and persist the resulting group state.
/// Production code uses `poll_mls_welcomes`; this command is exposed for
/// manual invocation or testing.
pub async fn process_welcome(
    state: &Arc<AppState>,
    welcome_bytes: Vec<u8>,
) -> Result<()> {
    apply_welcome(state, &welcome_bytes).await
}

/// Poll the remote `mls_welcome` table for undelivered Welcome messages
/// addressed to `user_id`.  Each one is applied locally and then marked
/// `delivered = 1` so it is not processed again.
///
/// Called on startup and from `poll_pending_messages`.
pub async fn poll_mls_welcomes_inner(state: &Arc<AppState>, user_id: &str, device_id: &str) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Fetch welcomes targeted at this specific device, plus legacy rows
    // (recipient_device_id IS NULL) from before multi-device was deployed.
    let mut rows = conn.query(
        "SELECT id, welcome_data FROM mls_welcome \
         WHERE recipient_id = ?1 AND delivered = 0 \
         AND (recipient_device_id = ?2 OR recipient_device_id IS NULL) \
         ORDER BY created_at ASC",
        libsql::params![user_id, device_id],
    ).await?;

    // Drain into owned Vec so `rows` is dropped before local-DB awaits below.
    let mut items: Vec<(String, Vec<u8>)> = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let bytes: Vec<u8> = row.get(1)?;
        items.push((id, bytes));
    }
    drop(rows);

    let had_welcomes = !items.is_empty();
    for (id, bytes) in items {
        match apply_welcome(state, &bytes).await {
            Ok(()) => {
                eprintln!("[mls] poll_mls_welcomes: applied welcome {id}");
            }
            Err(e) => {
                // Mark as delivered even on failure — the private key for this
                // Welcome was likely orphaned by a DB wipe and will never
                // succeed. The repair mechanism will generate a new Welcome.
                eprintln!("[mls] poll_mls_welcomes: failed to apply welcome {id}: {e}");
            }
        }

        let _ = conn.execute(
            "UPDATE mls_welcome SET delivered = 1 WHERE id = ?1",
            libsql::params![id],
        ).await;
    }

    // Each processed welcome consumed a KP — top back up to TARGET.
    if had_welcomes {
        if let Err(e) = replenish_key_packages(state, user_id, device_id).await {
            eprintln!("[mls] KP replenishment failed (non-fatal): {e}");
        }
    }

    Ok(())
}

pub async fn poll_mls_welcomes(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<()> {
    let device_id = state.device_id.lock().await.clone()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("device_id not set")))?;
    poll_mls_welcomes_inner(state, &user_id, &device_id).await
}
