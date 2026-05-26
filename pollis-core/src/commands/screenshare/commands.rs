//! The "always compiled" subscribe-side commands. Capture lifecycle
//! commands live in the per-platform `start_unix` / `start_windows` /
//! `unsupported` modules.

use std::sync::Arc;

use crate::{error::Result, sink::EventSink, state::AppState};

use super::{RawSink, ScreenShareEvent};

pub async fn subscribe_screen_share_events(
    sink: Arc<dyn EventSink<ScreenShareEvent>>,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut ss = state.screenshare.lock().await;
    ss.events = Some(sink);
    Ok(())
}

pub async fn subscribe_screen_share_frames(
    sink: Arc<dyn RawSink>,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut ss = state.screenshare.lock().await;
    ss.frames = Some(sink);
    Ok(())
}
