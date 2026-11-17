//! Device hot-plug (#1989): audio-server events and lost streams, reconciled
//! against the live call. Event-driven; nothing here polls.
//!
//! Two sources feed one supervisor task:
//! - Linux with a sound server: a thread subscribed to the PulseAudio protocol
//!   (pipewire-pulse speaks it too) for sinks and sources appearing or going
//!   and for the server's default changing.
//! - Every host: a voice stream's error callback reporting its device gone.
//!
//! The supervisor tells the renderer (`VoiceEvent::AudioDevicesChanged`, so
//! the settings page refetches) and then makes the open mic and speaker match
//! what the user's choice resolves to now: a call on "Default" moves to the new
//! default (AirPods connected), a device that went away falls back to the
//! default, and a chosen device that comes back is picked up again. On ALSA
//! with no server there are no notifications; only lost streams arrive.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, OnceLock, Weak,
};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::state::AppState;

use super::devices::{reset_host, resolved_id, DEFAULT_DEVICE};
use super::streams::StreamErrorHook;
use super::types::{OpenedDevice, VoiceEvent};

/// One thing that happened to the audio devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signal {
    /// A device appeared or went, or the server's default changed.
    DevicesChanged,
    /// The server connection the watch held died: the server restarted.
    ServerLost,
    /// A voice stream reported its device gone.
    StreamLost { input: bool, generation: u64 },
}

/// A server announces one hot-plug as a burst (card, sink, source, default):
/// act once, after the burst.
const SETTLE: Duration = Duration::from_millis(250);

/// A stream lost again this soon after its reopen is not reopened again: the
/// device keeps failing, and reopening it in a loop helps nobody. The next
/// device change still reconciles it.
const RELOST_WINDOW: Duration = Duration::from_secs(2);

static SIGNALS: OnceLock<UnboundedSender<Signal>> = OnceLock::new();

/// Each voice stream's generation; an error from a stream already replaced
/// carries an older one and is ignored.
static GENERATION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// The error hook for a voice stream: a lost device becomes a reopen.
pub(crate) fn stream_hook(input: bool, generation: u64) -> StreamErrorHook {
    Arc::new(move |e: &cpal::Error| {
        if !is_lost(e.kind()) {
            return;
        }
        if let Some(tx) = SIGNALS.get() {
            let _ = tx.send(Signal::StreamLost { input, generation });
        }
    })
}

/// Errors after which the stream delivers nothing until it is rebuilt.
fn is_lost(kind: cpal::ErrorKind) -> bool {
    matches!(
        kind,
        cpal::ErrorKind::DeviceNotAvailable
            | cpal::ErrorKind::StreamInvalidated
            | cpal::ErrorKind::HostUnavailable
    )
}

/// Start watching, once per process. Called when the renderer subscribes to
/// voice events (app start), so the picker hears hot-plugs outside a call too.
pub(crate) fn start(state: &Arc<AppState>) {
    SIGNALS.get_or_init(|| {
        let (tx, rx) = unbounded_channel();
        tokio::spawn(supervise(Arc::downgrade(state), rx));
        tx
    });
    ensure_server_watch();
}

/// (Re)start the audio-server subscription if it is not running: it ends when
/// the server restarts, and comes back on the next settings-page listing or
/// voice event subscription (lazy recovery, no retry timer).
pub(crate) fn ensure_server_watch() {
    #[cfg(target_os = "linux")]
    server::ensure();
}

async fn supervise(state: Weak<AppState>, mut rx: UnboundedReceiver<Signal>) {
    let mut reopened_at: [Option<Instant>; 2] = [None, None];
    let mut default_output: Option<String> = None;
    while let Some(first) = rx.recv().await {
        let mut batch = vec![first];
        let settle = tokio::time::sleep(SETTLE);
        tokio::pin!(settle);
        loop {
            tokio::select! {
                next = rx.recv() => match next {
                    Some(s) => batch.push(s),
                    None => break,
                },
                _ = &mut settle => break,
            }
        }
        let Some(state) = state.upgrade() else {
            return;
        };
        handle(&state, &batch, &mut reopened_at, &mut default_output).await;
    }
}

/// What one batch of signals asks for.
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    /// The device list may differ: tell the renderer.
    announce: bool,
    /// Reconnect the host first.
    reset_host: bool,
    /// Generations of mic / speaker streams that reported their device lost.
    lost_input: Vec<u64>,
    lost_output: Vec<u64>,
}

fn plan(batch: &[Signal]) -> Plan {
    let mut plan = Plan::default();
    for signal in batch {
        match *signal {
            Signal::DevicesChanged => plan.announce = true,
            Signal::ServerLost => {
                plan.announce = true;
                plan.reset_host = true;
            }
            Signal::StreamLost { input: true, generation } => plan.lost_input.push(generation),
            Signal::StreamLost { input: false, generation } => plan.lost_output.push(generation),
        }
    }
    plan
}

async fn handle(
    state: &Arc<AppState>,
    batch: &[Signal],
    reopened_at: &mut [Option<Instant>; 2],
    default_output: &mut Option<String>,
) {
    let plan = plan(batch);
    if plan.reset_host {
        let _ = tokio::task::spawn_blocking(reset_host).await;
    }
    if plan.announce {
        let channel = state.voice.lock().await.channel.clone();
        if let Some(ch) = channel {
            let _ = ch.send(VoiceEvent::AudioDevicesChanged);
        }
    }

    // What is open now, and what the user chose.
    let (mic, desired_in, speaker, desired_out) = {
        let voice = state.voice.lock().await;
        let mic = voice.frame_task.is_some().then(|| voice.input_device.clone());
        let pb = voice.playback.lock().unwrap_or_else(|p| p.into_inner());
        let speaker = pb.output_stream.is_some().then(|| pb.output_device.clone());
        (mic, voice.current_input_device.clone(), speaker, pb.output_device_name.clone())
    };

    // Where each would open now (blocking: one server round trip each).
    let want_mic = mic.is_some();
    let want_speaker = speaker.is_some();
    let (desired_in_q, desired_out_q) = (desired_in.clone(), desired_out.clone());
    let Ok((target_in, target_out, new_default_out)) = tokio::task::spawn_blocking(move || {
        let target_in = want_mic.then(|| resolved_id(desired_in_q.as_deref(), true)).flatten();
        let target_out = want_speaker.then(|| resolved_id(desired_out_q.as_deref(), false)).flatten();
        (target_in, target_out, resolved_id(None, false))
    })
    .await
    else {
        return;
    };

    // Sound effects always play on the default output.
    if default_output.is_some() && *default_output != new_default_out {
        crate::commands::sfx::reset_output();
    }
    *default_output = new_default_out;

    if let Some(open) = mic {
        if needs_reopen(&open, target_in.as_deref(), &plan.lost_input, &mut reopened_at[0], "mic") {
            let choice = desired_in.unwrap_or_else(|| DEFAULT_DEVICE.to_string());
            if let Err(e) = super::lifecycle::set_voice_input_device(choice, state).await {
                crate::diag!("[voice] mic reopen after a device change failed: {e}");
            }
        }
    }
    if let Some(open) = speaker {
        if needs_reopen(&open, target_out.as_deref(), &plan.lost_output, &mut reopened_at[1], "speaker") {
            if let Err(e) = super::playback::ensure_playback(Arc::clone(&state.voice), desired_out, true).await {
                crate::diag!("[voice] speaker reopen after a device change failed: {e}");
            }
        }
    }
}

/// Whether the stream `open` must be rebuilt: its device lost (and not lost
/// again right after the last reopen), or the choice now resolves elsewhere.
fn needs_reopen(
    open: &OpenedDevice,
    target: Option<&str>,
    lost: &[u64],
    reopened_at: &mut Option<Instant>,
    what: &str,
) -> bool {
    let lost = lost.contains(&open.generation);
    let moved = target.is_some() && target != open.id.as_deref();
    if !lost && !moved {
        return false;
    }
    if lost && !moved && reopened_at.is_some_and(|at| at.elapsed() < RELOST_WINDOW) {
        crate::diag!("[voice] {what} device failed again right after its reopen; leaving it");
        return false;
    }
    crate::diag!(
        "[voice] {what} {}; reopening",
        if moved { "choice now resolves to another device" } else { "device lost" }
    );
    *reopened_at = Some(Instant::now());
    true
}

/// The PulseAudio-protocol subscription (Linux with a sound server).
#[cfg(target_os = "linux")]
mod server {
    use std::ffi::CString;
    use std::io::BufReader;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use pulseaudio::protocol::{
        self, Command, SubscriptionEventFacility, SubscriptionEventType, SubscriptionMask,
    };

    use super::{Signal, SIGNALS};

    static RUNNING: AtomicBool = AtomicBool::new(false);

    /// The handshake has a deadline; the event wait after it does not.
    const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

    pub(super) fn ensure() {
        let Some(tx) = SIGNALS.get() else {
            return;
        };
        if pulseaudio::socket_path_from_env().is_none() {
            return;
        }
        if RUNNING.swap(true, Ordering::AcqRel) {
            return;
        }
        let tx = tx.clone();
        let spawned = std::thread::Builder::new()
            .name("pollis-audio-watch".into())
            .spawn(move || {
                let ended = watch(&|| tx.send(Signal::DevicesChanged).is_ok());
                RUNNING.store(false, Ordering::Release);
                if let Err(e) = ended {
                    crate::diag!("[voice] audio-server watch ended: {e}");
                    let _ = tx.send(Signal::ServerLost);
                }
            });
        if spawned.is_err() {
            RUNNING.store(false, Ordering::Release);
        }
    }

    /// Whether an event can change the device list or the default. Sinks and
    /// sources also report `Changed` on every volume change and every stream
    /// start; those are skipped.
    pub(super) fn is_device_event(facility: SubscriptionEventFacility, kind: SubscriptionEventType) -> bool {
        match facility {
            SubscriptionEventFacility::Sink | SubscriptionEventFacility::Source => {
                kind != SubscriptionEventType::Changed
            }
            SubscriptionEventFacility::Server => true,
            _ => false,
        }
    }

    /// Subscribe and block on events until the server goes away (`Err`) or
    /// `notify` reports nobody is listening (`Ok`).
    pub(super) fn watch(notify: &dyn Fn() -> bool) -> Result<(), String> {
        let path = pulseaudio::socket_path_from_env().ok_or("no server socket")?;
        let socket = UnixStream::connect(path).map_err(|e| e.to_string())?;
        socket.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).map_err(|e| e.to_string())?;
        let mut sock = BufReader::new(socket);

        let cookie = pulseaudio::cookie_path_from_env()
            .and_then(|p| std::fs::read(p).ok())
            .unwrap_or_default();
        let auth = protocol::AuthParams {
            version: protocol::MAX_VERSION,
            supports_shm: false,
            supports_memfd: false,
            cookie,
        };
        protocol::write_command_message(sock.get_mut(), 0, &Command::Auth(auth), protocol::MAX_VERSION)
            .map_err(|e| e.to_string())?;
        let (_, reply) = protocol::read_reply_message::<protocol::AuthReply>(&mut sock, protocol::MAX_VERSION)
            .map_err(|e| e.to_string())?;
        let version = std::cmp::min(protocol::MAX_VERSION, reply.version);

        let mut props = protocol::Props::new();
        let name = CString::new("pollis-device-watch").map_err(|e| e.to_string())?;
        props.set(protocol::Prop::ApplicationName, name);
        protocol::write_command_message(sock.get_mut(), 1, &Command::SetClientName(props), version)
            .map_err(|e| e.to_string())?;
        protocol::read_reply_message::<protocol::SetClientNameReply>(&mut sock, version)
            .map_err(|e| e.to_string())?;

        let mask = SubscriptionMask::SINK | SubscriptionMask::SOURCE | SubscriptionMask::SERVER;
        protocol::write_command_message(sock.get_mut(), 2, &Command::Subscribe(mask), version)
            .map_err(|e| e.to_string())?;
        protocol::read_ack_message(&mut sock).map_err(|e| e.to_string())?;
        sock.get_ref().set_read_timeout(None).map_err(|e| e.to_string())?;
        crate::diag!("[voice] watching the audio server for device changes");

        loop {
            let (_, command) = protocol::read_command_message(&mut sock, version).map_err(|e| e.to_string())?;
            let Command::SubscribeEvent(event) = command else {
                continue;
            };
            if is_device_event(event.event_facility, event.event_type) && !notify() {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(id: &str, generation: u64) -> OpenedDevice {
        OpenedDevice { id: Some(id.into()), generation }
    }

    #[test]
    fn a_burst_plans_one_announcement_and_collects_lost_streams() {
        let batch = [
            Signal::DevicesChanged,
            Signal::StreamLost { input: true, generation: 7 },
            Signal::DevicesChanged,
            Signal::StreamLost { input: false, generation: 9 },
        ];
        assert_eq!(
            plan(&batch),
            Plan { announce: true, reset_host: false, lost_input: vec![7], lost_output: vec![9] }
        );
        let lost_only = plan(&[Signal::StreamLost { input: true, generation: 3 }]);
        assert!(!lost_only.announce, "a lost stream alone does not change the list");
        assert!(plan(&[Signal::ServerLost]).reset_host);
    }

    #[test]
    fn a_default_call_moves_when_the_default_changes() {
        let mut at = None;
        let open = opened("pulseaudio:alsa_output.pci.analog-stereo", 1);
        assert!(needs_reopen(&open, Some("pulseaudio:bluez_output.AA_BB.1"), &[], &mut at, "speaker"));
        assert!(at.is_some());
    }

    #[test]
    fn nothing_moves_when_the_choice_still_resolves_to_the_open_device() {
        let mut at = None;
        let open = opened("pulseaudio:alsa_input.usb-Yeti", 1);
        assert!(!needs_reopen(&open, Some("pulseaudio:alsa_input.usb-Yeti"), &[], &mut at, "mic"));
        // No device at all right now: keep what is open rather than tear it down.
        assert!(!needs_reopen(&open, None, &[], &mut at, "mic"));
    }

    #[test]
    fn a_lost_stream_is_reopened_but_only_its_own_generation() {
        let mut at = None;
        let open = opened("coreaudio:usb-mic", 4);
        assert!(!needs_reopen(&open, Some("coreaudio:usb-mic"), &[3], &mut at, "mic"), "an older stream's error");
        assert!(needs_reopen(&open, Some("coreaudio:usb-mic"), &[4], &mut at, "mic"));
    }

    #[test]
    fn a_device_that_fails_again_right_after_its_reopen_is_left_alone() {
        let mut at = Some(Instant::now());
        let open = opened("coreaudio:usb-mic", 5);
        assert!(!needs_reopen(&open, Some("coreaudio:usb-mic"), &[5], &mut at, "mic"));
        // But a move to another device still happens.
        assert!(needs_reopen(&open, Some("coreaudio:builtin"), &[5], &mut at, "mic"));
    }

    /// Against this machine's audio server: adding and removing a virtual sink
    /// (`pactl load-module module-null-sink`, no real device and no default
    /// touched) arrives as device events. Needs `pactl`.
    /// `cargo test -p pollis-core --lib device_watch::tests::a_virtual_sink_coming_and_going_is_heard -- --ignored --nocapture`
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "talks to this machine's audio server"]
    fn a_virtual_sink_coming_and_going_is_heard() {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<()>();
        std::thread::spawn(move || server::watch(&|| tx.send(()).is_ok()));
        // Let the subscription land before the change.
        std::thread::sleep(Duration::from_millis(500));
        let pactl = |args: &[&str]| {
            let out = std::process::Command::new("pactl").args(args).output().expect("pactl");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let drain = || std::iter::from_fn(|| rx.recv_timeout(Duration::from_millis(500)).ok()).count();
        let module = pactl(&["load-module", "module-null-sink", "sink_name=pollis_watch_probe"]);
        let added = drain();
        pactl(&["unload-module", &module]);
        let removed = drain();
        println!("events: {added} on add, {removed} on remove");
        assert!(added > 0 && removed > 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_device_and_default_events_count() {
        use pulseaudio::protocol::{SubscriptionEventFacility as F, SubscriptionEventType as T};
        assert!(server::is_device_event(F::Sink, T::New));
        assert!(server::is_device_event(F::Source, T::Removed));
        assert!(server::is_device_event(F::Server, T::Changed));
        assert!(!server::is_device_event(F::Sink, T::Changed), "volume changes and stream starts");
        assert!(!server::is_device_event(F::SinkInput, T::New));
        assert!(!server::is_device_event(F::Card, T::Changed));
    }
}
