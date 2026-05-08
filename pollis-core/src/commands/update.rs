use std::sync::atomic::Ordering;
use std::sync::Arc;


use crate::error::Result;
use crate::state::AppState;

pub async fn mark_update_required(state: &Arc<AppState>) -> Result<()> {
    state.update_required.store(true, Ordering::Relaxed);
    Ok(())
}

pub async fn is_update_required(state: &Arc<AppState>) -> Result<bool> {
    Ok(state.update_required.load(Ordering::Relaxed))
}
