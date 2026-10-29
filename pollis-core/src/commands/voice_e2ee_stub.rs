//! Mobile stub for voice E2EE: no voice keys exist on mobile, so
//! `on_mls_epoch_changed` (called from ungated MLS code) is a no-op.

use std::sync::Arc;

use crate::state::AppState;

pub async fn on_mls_epoch_changed(_state: &Arc<AppState>, _mls_group_id: &str) {}
