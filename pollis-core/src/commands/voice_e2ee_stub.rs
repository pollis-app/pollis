//! Mobile (iOS/Android) stub for voice end-to-end key management.
//!
//! No voice keys on mobile, so `on_mls_epoch_changed` is a no-op.

use std::sync::Arc;

use crate::state::AppState;

pub async fn on_mls_epoch_changed(_state: &Arc<AppState>, _mls_group_id: &str) {}
