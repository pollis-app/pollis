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
// Original, synthesized by scripts/gen-ringback-wav.py (#1901).
static RINGBACK_CLIP: &[u8] = include_bytes!("../../../assets/sfx/ringback.wav");
static MUTE: &[u8] = include_bytes!("../../../assets/sfx/mute_fx.wav");
static UNMUTE: &[u8] = include_bytes!("../../../assets/sfx/unmute_fx.wav");
static DEAFEN: &[u8] = include_bytes!("../../../assets/sfx/deafen_fx.wav");
static UNDEAFEN: &[u8] = include_bytes!("../../../assets/sfx/undeafen_fx.wav");

// One process-wide output on a parked thread (`OutputStream` is `!Send`):
// cold opens can take seconds after idle. `None` = no audio, silent no-op.
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
                        // `stream` is !Send and must outlive every Sink; park forever.
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

/// Prewarm audio so the first ringtone skips the cold open. Idempotent.
pub fn prewarm_audio() {
    std::thread::spawn(|| {
        let _ = ensure_audio();
    });
}

/// Play a named sound effect on the host audio device.
pub fn play_sfx(sound: &str) {
    let bytes: &'static [u8] = match sound {
        "ping" => PING,
        "join" => JOIN,
        "leave" => LEAVE,
        "mute" => MUTE,
        "unmute" => UNMUTE,
        "deafen" => DEAFEN,
        "undeafen" => UNDEAFEN,
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

/// A clip played on a loop until stopped. Each wav carries its own silence, so
/// looping the whole clip produces the ring-pause-ring pattern. One slot per
/// tone: the incoming ringtone and the caller's ringback never stop each other.
struct LoopingTone {
    name: &'static str,
    clip: &'static [u8],
    stop: Mutex<Option<Arc<AtomicBool>>>,
}

impl LoopingTone {
    const fn new(name: &'static str, clip: &'static [u8]) -> Self {
        Self {
            name,
            clip,
            stop: Mutex::new(None),
        }
    }

    /// Claim the slot. `None` while the tone is already playing.
    fn arm(&self) -> Option<Arc<AtomicBool>> {
        let mut guard = self.stop.lock().ok()?;
        if guard.is_some() {
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        *guard = Some(Arc::clone(&stop));
        Some(stop)
    }

    /// Start looping. Idempotent: a second start while playing is a no-op.
    fn start(&'static self) {
        let Some(handle) = ensure_audio() else {
            return;
        };
        let Some(stop) = self.arm() else {
            return;
        };
        let handle = handle.clone();
        std::thread::spawn(move || {
            let sink = match rodio::Sink::try_new(&handle) {
                Ok(s) => s,
                Err(e) => {
                    crate::diag!("[sfx] {} Sink::try_new failed: {e}", self.name);
                    return;
                }
            };
            while !stop.load(Ordering::Acquire) {
                let source = match rodio::Decoder::new(Cursor::new(self.clip)) {
                    Ok(s) => s,
                    Err(e) => {
                        crate::diag!("[sfx] {} Decoder failed: {e}", self.name);
                        return;
                    }
                };
                sink.append(source);
                // Poll so a stop lands within ~100 ms.
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

    /// Stop looping, if playing. Idempotent.
    fn stop(&self) {
        let mut guard = match self.stop.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Some(stop) = guard.take() {
            stop.store(true, Ordering::Release);
        }
    }
}

/// The callee's incoming-call ringtone.
static RING: LoopingTone = LoopingTone::new("ring", CALL);
/// The caller's ringback while a 1:1 DM call rings the other side (#1901).
static RINGBACK: LoopingTone = LoopingTone::new("ringback", RINGBACK_CLIP);

/// Start the incoming-call ringtone on a loop. Idempotent.
pub fn start_ring() {
    RING.start();
}

/// Stop the incoming-call ringtone, if any. Idempotent.
pub fn stop_ring() {
    RING.stop();
}

/// Start the caller's ringback on a loop, on the same output as every other
/// sound effect. Idempotent.
pub fn start_ringback() {
    RINGBACK.start();
}

/// Stop the caller's ringback, if any. Idempotent.
pub fn stop_ringback() {
    RINGBACK.stop();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_arms_once_until_stopped() {
        let tone = LoopingTone::new("test", RINGBACK_CLIP);
        let first = tone.arm().expect("an idle tone arms");
        assert!(tone.arm().is_none(), "a playing tone must not start a second loop");
        tone.stop();
        assert!(first.load(Ordering::Acquire), "stop signals the running loop");
        assert!(tone.arm().is_some(), "a stopped tone can start again");
    }

    #[test]
    fn stopping_an_idle_tone_is_a_no_op() {
        let tone = LoopingTone::new("test", RINGBACK_CLIP);
        tone.stop();
        assert!(tone.arm().is_some());
    }

    #[test]
    fn ring_and_ringback_have_separate_slots() {
        let ring = LoopingTone::new("ring", CALL);
        let ringback = LoopingTone::new("ringback", RINGBACK_CLIP);
        let _ring = ring.arm().expect("ring arms");
        assert!(ringback.arm().is_some(), "ringing must not block the ringback");
        ringback.stop();
        assert!(ring.arm().is_none(), "stopping the ringback leaves the ring playing");
    }

    #[test]
    fn the_ringback_clip_decodes() {
        let source = rodio::Decoder::new(Cursor::new(RINGBACK_CLIP)).expect("ringback.wav decodes");
        assert_eq!(rodio::Source::channels(&source), 1);
    }
}
