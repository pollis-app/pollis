//! Trigger claims (#1182, job loop step 5): several hosts of one bot see every
//! trigger, and the DS's `UNIQUE (installation_id, trigger_key)` decides which
//! of them acts. Event-driven: one call per trigger, no leases or keepalives.

use std::sync::Arc;

use pollis_api::bots::ClaimTriggerBody;

use crate::error::Result;
use crate::state::AppState;

/// Claim `(installation_id, trigger_key)` for this host, device-signed.
/// `true` when this host won; a retry by the winner answers `true` again, so
/// a job that failed after winning can run again without losing its claim.
pub async fn claim_trigger(state: &Arc<AppState>, installation_id: &str, trigger_key: &str) -> Result<bool> {
    let body = ClaimTriggerBody {
        installation_id: installation_id.to_string(),
        trigger_key: trigger_key.to_string(),
    };
    Ok(crate::commands::mls::ds_post_json(state, &body).await?.won)
}
