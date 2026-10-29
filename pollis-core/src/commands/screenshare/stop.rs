//! Shared stop path: drain state, fence the pump, unpublish, then drop the source.
//! Safe when nothing is live.

use std::sync::Arc;

use crate::{error::Result, state::AppState};

use super::ScreenShareEvent;

pub async fn stop_screen_share(state: &Arc<AppState>) -> Result<()> {
    let room;
    let track;
    let source_to_drop;
    let audio_track;
    let audio_source_to_drop;
    let ev_opt;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let mut helper;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let reader;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let mut picker;
    #[cfg(target_os = "windows")]
    let windows_thread;
    #[cfg(target_os = "windows")]
    let windows_active;
    {
        let mut ss = state.screenshare.lock().await;
        track = ss.local_track.take();
        // Hold the source until the track is unpublished (in-flight handler calls).
        source_to_drop = ss.local_source.take();
        // Same rule for the audio pair.
        audio_track = ss.local_audio_track.take();
        audio_source_to_drop = ss.local_audio_source.take();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            helper = ss.local_helper.take();
            reader = ss.local_reader_task.take();
            picker = ss.picker_session.take();
            // Closing the writer sends the helper EOF so it exits promptly.
            ss.local_writer = None;
        }
        #[cfg(target_os = "windows")]
        {
            windows_thread = ss.windows_thread.take();
            windows_active = ss.windows_active.take();
        }
        ev_opt = ss.events.clone();
        let voice = state.voice.lock().await;
        room = voice.room.clone();
    }

    // Nothing live: skip LocalStopped, which would clobber a starting share's UI.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let had_session = track.is_some()
        || source_to_drop.is_some()
        || helper.is_some()
        || reader.is_some()
        || picker.is_some();
    #[cfg(target_os = "windows")]
    let had_session =
        track.is_some() || source_to_drop.is_some() || windows_thread.is_some();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let had_session = track.is_some() || source_to_drop.is_some();
    if !had_session {
        return Ok(());
    }

    // Disarm the self-echo tap; every stop path converges here.
    {
        let voice = state.voice.lock().await;
        super::self_echo::disarm(&voice.shared_audio_render);
    }

    // Linux + macOS: abort the reader, kill the helper (which also stops SCK).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if let Some(t) = reader {
            t.abort();
        }
        if let Some(h) = helper.as_mut() {
            let _ = h.kill().await;
        }
        // Also kill an orphaned picker-phase helper.
        if let Some(p) = picker.as_mut() {
            let _ = p.child.kill().await;
        }
    }
    #[cfg(target_os = "windows")]
    {
        // 1. Fence the WGC callback (pairs with on_frame_arrived's Acquire); ends capture.
        if let Some(active) = &windows_active {
            active.store(false, std::sync::atomic::Ordering::Release);
        }
        // 2. Detach, don't join: the fence keeps it off the source; a join could hang.
        drop(windows_thread);
    }
    // 3. Unpublish before dropping the source (unpublish can free its backing).
    if let Some(room) = room {
        if let Some(track) = track {
            let sid = track.sid();
            if let Err(e) = room.local_participant().unpublish_track(&sid).await {
                crate::diag!("[screenshare] unpublish error: {e}");
            }
        }
        if let Some(track) = audio_track {
            let sid = track.sid();
            if let Err(e) = room.local_participant().unpublish_track(&sid).await {
                crate::diag!("[screenshare/audio] unpublish error: {e}");
            }
        }
    }
    // 4. Now the sources can be dropped safely.
    drop(source_to_drop);
    drop(audio_source_to_drop);
    if let Some(ev) = ev_opt {
        let _ = ev.send(ScreenShareEvent::LocalStopped);
    }
    Ok(())
}
