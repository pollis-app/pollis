use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rodio::{OutputStream, OutputStreamHandle};

static PING: &[u8] = include_bytes!("../../../assets/sfx/ping_fx.wav");
static JOIN: &[u8] = include_bytes!("../../../assets/sfx/join_fx.wav");
static LEAVE: &[u8] = include_bytes!("../../../assets/sfx/leave-fx.wav");
static CALL: &[u8] = include_bytes!("../../../assets/sfx/call.wav");

// Process-wide audio output, opened once and reused: a cold
// `OutputStream::try_default()` can take seconds after the audio stack idles
// (Linux, Windows), which delayed the ringtone. `OutputStream` is `!Send`, so it
// lives on a parked thread; callers borrow the `Send + Sync` handle. `None` means
// the backend failed to open and every call no-ops.
static AUDIO_HANDLE: OnceLock<Option<OutputStreamHandle>> = OnceLock::new();

fn ensure_audio() -> Option<&'static OutputStreamHandle> {
    AUDIO_HANDLE
        .get_or_init(|| {
            let (tx, rx) = sync_channel::<Option<OutputStreamHandle>>(1);
            let spawn_result = std::thread::Builder::new()
                .name("pollis-audio".into())
                .spawn(move || match OutputStream::try_default() {
                    Ok((stream, handle)) => {
                        let _ = tx.send(Some(handle));
                        // `stream` must outlive every Sink created from `handle`; this thread never
                        // exits.
                        let _keep_alive = stream;
                        loop {
                            std::thread::park();
                        }
                    }
                    Err(e) => {
                        crate::diag!("[sfx] OutputStream::try_default failed: {e}");
                        let _ = tx.send(None);
                    }
                });
            if let Err(e) = spawn_result {
                crate::diag!("[sfx] failed to spawn audio thread: {e}");
                return None;
            }
            rx.recv().ok().flatten()
        })
        .as_ref()
}

/// Prewarm the audio backend so the first playback (usually the ringtone) skips
/// the cold-open cost. Idempotent.
pub fn prewarm_audio() {
    std::thread::spawn(|| {
        let _ = ensure_audio();
    });
}

/// Play a named sound effect. Errors are ignored: a missing audio device must
/// never crash the app.
pub fn play_sfx(sound: &str) {
    let bytes: &'static [u8] = match sound {
        "ping" => PING,
        "join" => JOIN,
        "leave" => LEAVE,
        _ => return,
    };

    let Some(handle) = ensure_audio() else {
        return;
    };
    let sink = match rodio::Sink::try_new(handle) {
        Ok(s) => s,
        Err(e) => {
            crate::diag!("[sfx] play_sfx Sink::try_new failed for {sound}: {e}");
            return;
        }
    };
    let source = match rodio::Decoder::new(Cursor::new(bytes)) {
        Ok(s) => s,
        Err(e) => {
            crate::diag!("[sfx] play_sfx Decoder failed for {sound}: {e}");
            return;
        }
    };
    sink.append(source);
    sink.detach();
}

/// Set while the ringtone loops. The clip has built-in silence, so looping it
/// gives a ring-pause-ring pattern.
static RING_STOP: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

/// Start the incoming-call ringtone on a loop. Idempotent.
pub fn start_ring() {
    let Some(handle) = ensure_audio() else {
        return;
    };

    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut guard = match RING_STOP.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if guard.is_some() {
            return;
        }
        *guard = Some(Arc::clone(&stop));
    }

    let handle = handle.clone();
    std::thread::spawn(move || {
        let sink = match rodio::Sink::try_new(&handle) {
            Ok(s) => s,
            Err(e) => {
                crate::diag!("[sfx] start_ring Sink::try_new failed: {e}");
                return;
            }
        };
        while !stop.load(Ordering::Acquire) {
            let source = match rodio::Decoder::new(Cursor::new(CALL)) {
                Ok(s) => s,
                Err(e) => {
                    crate::diag!("[sfx] start_ring Decoder failed: {e}");
                    return;
                }
            };
            sink.append(source);
            // Check the stop flag while the clip plays so a stop lands within ~100ms.
            while !sink.empty() && !stop.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(100));
            }
            if stop.load(Ordering::Acquire) {
                sink.stop();
                break;
            }
        }
    });
}

/// Stop the incoming-call ringtone, if any. Idempotent.
pub fn stop_ring() {
    let mut guard = match RING_STOP.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if let Some(stop) = guard.take() {
        stop.store(true, Ordering::Release);
    }
}
