use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, sync_channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::mixer::Mixer;
use rodio::{DeviceSinkBuilder, Player};

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

/// The open output: the mixer sounds play into, and the handle whose drop
/// releases the device on the thread that owns it.
struct Output {
    mixer: Mixer,
    _release: Sender<()>,
}

// One process-wide output on its own parked thread (the cpal stream inside is
// `!Send` on WASAPI): cold opens can take seconds after idle. Opened on first
// use; `reset_output` closes it when the default output changes (#1989), and
// the next sound reopens it on the new default.
static OUTPUT: Mutex<Option<Output>> = Mutex::new(None);

fn ensure_audio() -> Option<Mixer> {
    let mut slot = OUTPUT.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(out) = slot.as_ref() {
        return Some(out.mixer.clone());
    }
    let (tx, rx) = sync_channel::<Option<Mixer>>(1);
    let (release, released) = channel::<()>();
    let spawn_result = std::thread::Builder::new()
        .name("pollis-audio".into())
        .spawn(move || {
            // The default output, as the voice speaker resolves it (honours PULSE_SINK).
            let opened = crate::commands::voice::open_device(None, false, |dev| {
                DeviceSinkBuilder::from_device(dev.clone())
                    .and_then(|b| b.open_stream())
                    .map_err(|e| anyhow::anyhow!("{e}").into())
            });
            match opened {
                Ok(opened) => {
                    let mut sink = opened.value;
                    sink.log_on_drop(false);
                    let _ = tx.send(Some(sink.mixer().clone()));
                    // Holds the device until `reset_output` drops the sender.
                    let _ = released.recv();
                    drop(sink);
                }
                Err(e) => {
                    crate::diag!("[sfx] opening the default output failed: {e}");
                    let _ = tx.send(None);
                }
            }
        });
    if let Err(e) = spawn_result {
        crate::diag!("[sfx] failed to spawn audio thread: {e}");
        return None;
    }
    let mixer = rx.recv().ok().flatten()?;
    *slot = Some(Output { mixer: mixer.clone(), _release: release });
    Some(mixer)
}

/// Close the output so the next sound opens the current default.
pub(crate) fn reset_output() {
    let closed = OUTPUT.lock().unwrap_or_else(|p| p.into_inner()).take();
    if closed.is_some() {
        crate::diag!("[sfx] default output changed; reopening on the next sound");
    }
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

    let Some(mixer) = ensure_audio() else {
        return;
    };
    let source = match rodio::Decoder::new(Cursor::new(bytes)) {
        Ok(s) => s,
        Err(e) => {
            crate::diag!("[sfx] play_sfx Decoder failed for {sound}: {e}");
            return;
        }
    };
    let player = Player::connect_new(&mixer);
    player.append(source);
    player.detach();
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
        let Some(mixer) = ensure_audio() else {
            return;
        };
        let Some(stop) = self.arm() else {
            return;
        };
        std::thread::spawn(move || {
            let sink = Player::connect_new(&mixer);
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
        assert_eq!(rodio::Source::channels(&source).get(), 1);
    }
}
