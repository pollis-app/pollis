//! Mobile (iOS/Android) stub for voice end-to-end key management.
//!
//! Voice is desktop-only on mobile builds. `on_mls_epoch_changed` is the only
//! entry point reached from non-gated core code (MLS group-state + reconcile),
//! so this stub keeps that one call site `#[cfg]`-free. No voice keys exist on
//! mobile, so an epoch change has nothing to rekey — this is a no-op.

use std::sync::Arc;

use crate::state::AppState;

pub async fn on_mls_epoch_changed(_state: &Arc<AppState>, _mls_group_id: &str) {}
