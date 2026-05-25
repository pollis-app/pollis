//! No-op stubs compiled on platforms that have no capture implementation
//! (anything that isn't Linux, macOS, or Windows). The frontend should
//! never reach these in production; returning empty/Err keeps the public
//! surface signature consistent across the cfg matrix.

use std::sync::Arc;

use crate::{error::Result, state::AppState};

pub async fn start_screen_share(
    _state: &Arc<AppState>,
    _selection: Option<pollis_capture_proto::Selection>,
) -> Result<()> {
    Err(crate::error::Error::Other(anyhow::anyhow!(
        "screen share is not implemented on this OS yet"
    )))
}

pub async fn enumerate_screen_sources(
    _state: &Arc<AppState>,
) -> Result<pollis_capture_proto::SourceList> {
    Ok(pollis_capture_proto::SourceList {
        displays: Vec::new(),
        windows: Vec::new(),
    })
}

pub async fn cancel_screen_share_picker(_state: &Arc<AppState>) -> Result<()> {
    Ok(())
}
