//! The stop path for every capture backend: drain state, fence the platform
//! pump, unpublish, then drop the sources. Safe to call when nothing is live.

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
        // Sources stay alive locally until their tracks are unpublished, so an
        // in-flight capture_frame cannot outlive its backing.
        source_to_drop = ss.local_source.take();
        audio_track = ss.local_audio_track.take();
        audio_source_to_drop = ss.local_audio_source.take();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            helper = ss.local_helper.take();
            reader = ss.local_reader_task.take();
            picker = ss.picker_session.take();
            // Closing our writer half makes the helper see EOF and exit even if
            // its parent-death poll is mid-sleep.
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

    // Nothing was live: return without a spurious LocalStopped, which would
    // flip the UI off just as a fresh share starts.
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

    // Stop the voice mixer feeding a render reference nobody reads; every stop
    // path converges here.
    {
        let voice = state.voice.lock().await;
        super::self_echo::disarm(&voice.shared_audio_render);
    }

    // Kill the helper; on macOS that is also the SCStream stop, since SCK lives
    // entirely in the helper.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if let Some(t) = reader {
            t.abort();
        }
        if let Some(h) = helper.as_mut() {
            let _ = h.kill().await;
        }
        // A picker-phase helper orphaned by a failed capture.
        if let Some(p) = picker.as_mut() {
            let _ = p.child.kill().await;
        }
    }
    #[cfg(target_os = "windows")]
    {
        // Fence the WGC callback (pairs with the Acquire in on_frame_arrived);
        // the next frame callback then calls InternalCaptureControl::stop().
        if let Some(active) = &windows_active {
            active.store(false, std::sync::atomic::Ordering::Release);
        }
        // Detach rather than join: a surface producing no frames would block
        // stop forever, and the fence keeps the thread off the source.
        drop(windows_thread);
    }
    // Unpublish before dropping the sources: track teardown can free their
    // webrtc backing.
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
    drop(source_to_drop);
    drop(audio_source_to_drop);
    if let Some(ev) = ev_opt {
        let _ = ev.send(ScreenShareEvent::LocalStopped);
    }
    Ok(())
}
