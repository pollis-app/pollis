//! Shared stop path for every capture backend. Drains state, fences the
//! per-platform pump, unpublishes the LiveKit track, then drops the source.
//! Safe to call when nothing is live (the defensive pre-share teardown
//! exercises that path).

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
        // Keep the source alive locally until after the SCK stream is fully
        // torn down + the track is unpublished. Releasing it from state now
        // would otherwise let the next reference drop free its backing
        // while in-flight handler calls are still firing.
        source_to_drop = ss.local_source.take();
        // Same ordering rule as the video pair below: hold the source
        // alive locally until after the track is unpublished, so an
        // in-flight capture_frame can't outlive its backing.
        audio_track = ss.local_audio_track.take();
        audio_source_to_drop = ss.local_audio_source.take();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            helper = ss.local_helper.take();
            reader = ss.local_reader_task.take();
            picker = ss.picker_session.take();
            // Dropping the writer closes our half of the socket so the
            // helper sees EOF and exits cleanly even if its parent-death
            // poll is mid-sleep.
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

    // Nothing was live (e.g. the defensive pre-share teardown, or an
    // on_room_disconnected with no active share). Return without firing a
    // spurious LocalStopped — that would flip the UI's share state off
    // right as a fresh share is starting.
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

    // Stop the voice mixer feeding a render reference to a canceller
    // nothing is reading any more. Unconditional and idempotent: the slot
    // is only ever armed by a share, so clearing it here is the one place
    // every stop path already converges on.
    {
        let voice = state.voice.lock().await;
        super::self_echo::disarm(&voice.shared_audio_render);
    }

    // Linux + macOS: identical teardown. Abort the reader task, then
    // kill the helper subprocess. On macOS this also tears down SCK —
    // it lives entirely in the helper now, so killing the helper IS the
    // SCStream stop + picker deactivate. The helper's own Drop /
    // signal-on-exit handling releases SCK; we no longer have to drive
    // remove_output_handler / SCContentSharingPicker::set_active from
    // this process (that code moved into `pollis-capture-macos`).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if let Some(t) = reader {
            t.abort();
        }
        if let Some(h) = helper.as_mut() {
            let _ = h.kill().await;
        }
        // A picker-phase helper that was orphaned (e.g. capture failed
        // before consuming the picker session) needs the same kill.
        if let Some(p) = picker.as_mut() {
            let _ = p.child.kill().await;
        }
    }
    #[cfg(target_os = "windows")]
    {
        // 1. Fence the WGC callback from touching the source (pairs with
        //    the Acquire load in on_frame_arrived). Only this session's
        //    flag — taken from state — is flipped. This is also what ends
        //    the capture: the next frame callback observes it and calls
        //    InternalCaptureControl::stop(), which unblocks the dedicated
        //    thread's start() and lets it return.
        if let Some(active) = &windows_active {
            active.store(false, std::sync::atomic::Ordering::Release);
        }
        // 2. Detach the capture thread rather than force-joining it. The
        //    fence above guarantees it can no longer touch the LiveKit
        //    source, so it's safe to unpublish/drop the source below
        //    without waiting; the thread tears down its own WGC + COM
        //    state and exits on the next frame. Joining here would risk
        //    blocking stop indefinitely if the captured surface produced
        //    no further frames.
        drop(windows_thread);
    }
    // 3. Unpublish the track before dropping the source. LiveKit's track
    //    teardown can free the source's webrtc backing; doing it in this
    //    order avoids the "unpublish frees backing, handler crashes" race.
    if let Some(room) = room {
        if let Some(track) = track {
            let sid = track.sid();
            if let Err(e) = room.local_participant().unpublish_track(&sid).await {
                eprintln!("[screenshare] unpublish error: {e}");
            }
        }
        if let Some(track) = audio_track {
            let sid = track.sid();
            if let Err(e) = room.local_participant().unpublish_track(&sid).await {
                eprintln!("[screenshare/audio] unpublish error: {e}");
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
