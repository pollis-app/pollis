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

// Process-wide audio output. `OutputStream` is `!Send`, so it lives on a
// dedicated parked thread that opens the host audio device once at first
// access and stays alive for the whole process. Every play_sfx / start_ring
// borrows the `Send + Sync` handle and creates a fresh `Sink` on it —
// avoiding the per-call cost of opening the audio device.
//
// Why this matters: on Linux (PulseAudio / PipeWire) and Windows after the
// audio stack idles, `OutputStream::try_default()` can take seconds to
// return on the first call after a sleep. The previous implementation
// opened a fresh stream on every play, which manifested as ~5–10s delay
// before the incoming-call ringtone started — and a silent failure when
// the cold-open hung past the window the user cared about. The handle is
// initialised once, lazily, and reused forever after.
//
// `None` means the audio backend failed to open and every subsequent call
// silently no-ops. A missing audio device must never crash the app.
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
                        // `stream` is !Send and must outlive every Sink
                        // created from `handle`. Bind it to a stack local
                        // and park forever — the variable is dropped only
                        // if this thread exits, which it never does.
                        let _keep_alive = stream;
                        loop {
                            std::thread::park();
                        }
                    }
                    Err(e) => {
                        eprintln!("[sfx] OutputStream::try_default failed: {e}");
                        let _ = tx.send(None);
                    }
                });
            if let Err(e) = spawn_result {
                eprintln!("[sfx] failed to spawn audio thread: {e}");
                return None;
            }
            rx.recv().ok().flatten()
        })
        .as_ref()
}

/// Prewarm the audio backend so the first user-facing playback (typically
/// an incoming-call ringtone) doesn't pay the cold-open cost. Call once at
/// app startup. Idempotent — the underlying `OnceLock` only initialises
/// once regardless of how many callers fire this.
pub fn prewarm_audio() {
    std::thread::spawn(|| {
        let _ = ensure_audio();
    });
}

/// Play a named sound effect on the host audio device.
///
/// `Sink::detach()` hands the sink off to rodio's internal mixer thread
/// so playback continues without a wrapper thread blocking on it. All
/// errors are silently ignored — a missing audio device should never
/// crash the app.
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
            eprintln!("[sfx] play_sfx Sink::try_new failed for {sound}: {e}");
            return;
        }
    };
    let source = match rodio::Decoder::new(Cursor::new(bytes)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[sfx] play_sfx Decoder failed for {sound}: {e}");
            return;
        }
    };
    sink.append(source);
    sink.detach();
}

/// Looping ringtone playback. The wav file has built-in inter-ring silence so
/// looping the whole clip produces a natural ring-pause-ring pattern.
static RING_STOP: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

/// Start the incoming-call ringtone on a loop. Idempotent — calling twice
/// while already ringing is a no-op.
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
                eprintln!("[sfx] start_ring Sink::try_new failed: {e}");
                return;
            }
        };
        while !stop.load(Ordering::Acquire) {
            let source = match rodio::Decoder::new(Cursor::new(CALL)) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[sfx] start_ring Decoder failed: {e}");
                    return;
                }
            };
            sink.append(source);
            // Poll the stop flag while this iteration plays so a stop
            // request is honored within ~100ms instead of waiting out the
            // whole clip.
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
