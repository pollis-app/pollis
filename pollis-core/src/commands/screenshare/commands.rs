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

/// WebSocket URL the renderer connects to for the native screenshare frame
/// stream, e.g. `ws://127.0.0.1:<port>/ws/screenshare/<token>`. `None` until
/// the loopback media server is up and a session token has been minted
/// (post-unlock). The Tauri render path uses this instead of the per-frame
/// IPC `Channel`; under Electron the renderer decodes via livekit-client and
/// never calls this.
pub async fn screenshare_ws_url(state: &Arc<AppState>) -> Result<Option<String>> {
    let port = *state.media_server_port.lock().await;
    let token = state.media_server_token.lock().await.clone();
    Ok(match (port, token) {
        (Some(port), Some(token)) => {
            Some(format!("ws://127.0.0.1:{port}/ws/screenshare/{token}"))
        }
        _ => None,
    })
}
