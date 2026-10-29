//! Windows stub for the in-app terminal pane.

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::sink::RawSink;
use crate::state::AppState;

fn unsupported() -> Error {
    Error::Other(anyhow::anyhow!(
        "in-app terminal pane is not yet supported on Windows (ConPTY backend not implemented)"
    ))
}

/// Placeholder so `AppState::terminals: HashMap<String, PtySession>` resolves
/// on Windows. Nothing ever gets inserted because `terminal_open` errors.
pub struct PtySession {
    _private: (),
}

pub async fn terminal_open(
    _rows: u16,
    _cols: u16,
    _on_output: Arc<dyn RawSink>,
    _state: &Arc<AppState>,
) -> Result<String> {
    Err(unsupported())
}

pub async fn terminal_ack(
    _terminal_id: String,
    _bytes: usize,
    _state: &Arc<AppState>,
) -> Result<()> {
    Ok(())
}

pub async fn terminal_write(
    _terminal_id: &str,
    _data: &[u8],
    _state: &Arc<AppState>,
) -> Result<()> {
    Err(unsupported())
}

pub async fn terminal_resize(
    _terminal_id: String,
    _rows: u16,
    _cols: u16,
    _state: &Arc<AppState>,
) -> Result<()> {
    Ok(())
}

pub async fn terminal_close(_terminal_id: String, _state: &Arc<AppState>) -> Result<()> {
    Ok(())
}
