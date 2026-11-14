//! Always-compiled subscribe-side commands.

use std::sync::Arc;

use crate::{error::Result, sink::EventSink, state::AppState};

use super::ScreenShareEvent;

pub async fn subscribe_screen_share_events(
    sink: Arc<dyn EventSink<ScreenShareEvent>>,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut ss = state.screenshare.lock().await;
    ss.events = Some(sink);
    Ok(())
}

/// The share resolution setting, changed live (#1902). The running share's pump scales its
/// next frame to the new ceiling, with no republish; on macOS the helper also re-sizes its
/// ScreenCaptureKit stream, so raising the ceiling brings back detail SCK had dropped.
/// `None`/`0` = the source's own size. Also holds with no share running; a start carries its
/// own value.
pub async fn set_screen_share_max_height(
    state: &Arc<AppState>,
    max_height: Option<u32>,
) -> Result<()> {
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut ss = state.screenshare.lock().await;
    ss.ceiling.set(max_height);
    crate::diag!("[screenshare] ceiling now {:?}", ss.ceiling.get());
    #[cfg(target_os = "macos")]
    if let Some(writer) = ss.local_writer.as_mut() {
        use tokio::io::AsyncWriteExt;
        let msg = pollis_capture_proto::encode_set_max_height(max_height.unwrap_or(0));
        // The pump still applies a lower ceiling; only a raise past SCK's size is lost.
        if let Err(e) = writer.write_all(&msg).await {
            crate::diag!("[screenshare] send SetMaxHeight: {e}");
        }
    }
    Ok(())
}

/// WebSocket URL for the native screenshare frame stream; `None` until ready.
pub async fn screenshare_ws_url(state: &Arc<AppState>) -> Result<Option<String>> {
    let port = *state.media_server_port.lock().await;
    let token = state.media_server_token.lock().await.clone();
    Ok(match (port, token) {
        (Some(port), Some(token)) => Some(format!("ws://127.0.0.1:{port}/ws/screenshare/{token}")),
        _ => None,
    })
}
