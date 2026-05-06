use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

static PING: &[u8] = include_bytes!("../../../assets/sfx/ping_fx.wav");
static JOIN: &[u8] = include_bytes!("../../../assets/sfx/join_fx.wav");
static LEAVE: &[u8] = include_bytes!("../../../assets/sfx/leave-fx.wav");
static CALL: &[u8] = include_bytes!("../../../assets/sfx/call.wav");

/// Play a named sound effect on the host audio device.
///
/// Runs on a detached thread so it never blocks the Tauri command thread.
/// All errors are silently ignored — a missing audio device should never
/// crash the app.
#[tauri::command]
pub fn play_sfx(sound: &str) {
    let bytes: &'static [u8] = match sound {
        "ping" => PING,
        "join" => JOIN,
        "leave" => LEAVE,
        _ => return,
    };

    let sound_name = sound.to_string();
    std::thread::spawn(move || {
        let (_stream, handle) = match rodio::OutputStream::try_default() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[sfx] OutputStream::try_default failed for {sound_name}: {e}");
                return;
            }
        };
        let sink = match rodio::Sink::try_new(&handle) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[sfx] Sink::try_new failed for {sound_name}: {e}");
                return;
            }
        };
        let source = match rodio::Decoder::new(Cursor::new(bytes)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[sfx] Decoder::new failed for {sound_name}: {e}");
                return;
            }
        };
        sink.append(source);
        sink.sleep_until_end();
    });
}

/// Looping ringtone playback. The wav file has built-in inter-ring silence so
/// looping the whole clip produces a natural ring-pause-ring pattern.
/// `OutputStream` is `!Send`, so the stream + sink live entirely inside the
/// playback thread; the outside world only ever flips an atomic flag.
static RING_STOP: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

/// Start the incoming-call ringtone on a loop. Idempotent — calling twice
/// while already ringing is a no-op.
#[tauri::command]
pub fn start_ring() {
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

    std::thread::spawn(move || {
        let (_stream, handle) = match rodio::OutputStream::try_default() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[sfx] start_ring OutputStream failed: {e}");
                return;
            }
        };
        let sink = match rodio::Sink::try_new(&handle) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[sfx] start_ring Sink failed: {e}");
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
            // Poll the stop flag while this iteration plays so a stop request
            // is honored within ~100ms instead of waiting out the whole clip.
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
#[tauri::command]
pub fn stop_ring() {
    let mut guard = match RING_STOP.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if let Some(stop) = guard.take() {
        stop.store(true, Ordering::Release);
    }
}
