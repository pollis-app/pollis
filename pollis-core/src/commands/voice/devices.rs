//! Audio devices (#1989): the one shared cpal host, the picker's list, and
//! resolving a saved choice to a device that opens.
//!
//! An id is a cpal `DeviceId` string (`pulseaudio:<sink/source name>`,
//! `coreaudio:<UID>`, `wasapi:<endpoint id>`, `alsa:<pcm>`), stable across
//! restarts and re-plugs. `"default"` (or no value) means the OS default.
//! Anything else (a pre-#1989 cpal 0.15 name, a device that is gone) resolves
//! by name where the old name was a description, else to the default: a saved
//! choice never fails an open.
//!
//! Everything here blocks (the PulseAudio host does a protocol round trip per
//! call; its first construction is a handshake of up to 2 s), so callers run
//! it on `spawn_blocking`, never on a tokio worker or under the voice lock.

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait};

use crate::error::Result;

use super::types::AudioDevice;

/// The saved value meaning "whatever the OS default is right now".
pub(crate) const DEFAULT_DEVICE: &str = "default";

/// The process's one cpal host. `None` until first use, and again after
/// `reset_host` (the audio server went away).
static HOST: Mutex<Option<Arc<cpal::Host>>> = Mutex::new(None);

/// The shared cpal host, connecting on first use. Linux: PulseAudio (which
/// also serves PipeWire desktops via pipewire-pulse) when its socket exists,
/// else ALSA. Blocking.
pub(crate) fn host() -> Arc<cpal::Host> {
    let mut slot = HOST.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(host) = slot.as_ref() {
        return Arc::clone(host);
    }
    let started = std::time::Instant::now();
    let host = Arc::new(cpal::default_host());
    crate::diag!(
        "[voice] audio host {} ready in {:.0}ms",
        host.id(),
        started.elapsed().as_secs_f64() * 1000.0
    );
    *slot = Some(Arc::clone(&host));
    host
}

/// Forget the shared host so the next `host()` reconnects: its server
/// connection died (the audio server restarted).
pub(crate) fn reset_host() {
    *HOST.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// Connect the host on a blocking thread now, so a join's mic open does not
/// pay for the handshake.
pub(crate) fn warm_host() {
    let ready = HOST.lock().unwrap_or_else(|p| p.into_inner()).is_some();
    if ready {
        return;
    }
    tokio::task::spawn_blocking(|| {
        let _ = host();
    });
}

/// Which listing rules apply; derived from the host so the rules are testable
/// on every platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostKind {
    /// Linux, through a PulseAudio or pipewire-pulse server.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Pulse,
    /// Linux with no sound server: raw ALSA PCMs.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Alsa,
    /// CoreAudio / WASAPI: every endpoint is a real device with a human name.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Native,
}

fn host_kind(host: &cpal::Host) -> HostKind {
    #[cfg(target_os = "linux")]
    {
        if host.id() == cpal::HostId::PulseAudio {
            return HostKind::Pulse;
        }
        HostKind::Alsa
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = host;
        HostKind::Native
    }
}

/// Whether a pre-#1989 saved value (a cpal 0.15 device name) can name a device
/// by its description. True on macOS/Windows, where 0.15 names were already the
/// human names 0.18 describes devices by. On Linux they were ALSA PCM names
/// (`pulse`, `pipewire`, `jack`, `hw:CARD=…`), which name no device on any
/// host, so a legacy Linux value always lands on the default.
const LEGACY_NAMES_ARE_DESCRIPTIONS: bool = !cfg!(target_os = "linux");

/// The indices of `entries` (id, name) the picker offers, in order.
///
/// - PulseAudio: every sink/source except monitors (`<sink>.monitor`, what a
///   speaker is playing, never a microphone the user means).
/// - ALSA (no sound server): one `plughw:` PCM per physical device, labelled by
///   its card/device description. Plugin PCMs (`default`, `pulse`, `pipewire`,
///   `jack`, `dmix`, `surround51`, …) route to a server or to the default
///   rather than to one device, and raw `hw:` refuses most formats; `plughw:`
///   converts. The default is the picker's own "Default" entry.
/// - CoreAudio / WASAPI: everything.
///
/// A repeated id (or, on ALSA, a repeated label) keeps its first entry.
fn pickable(kind: HostKind, entries: &[(String, String)]) -> Vec<usize> {
    let mut seen_ids = std::collections::HashSet::new();
    let mut seen_names = std::collections::HashSet::new();
    let mut keep = Vec::new();
    for (i, (id, name)) in entries.iter().enumerate() {
        let offered = match kind {
            HostKind::Pulse => !id.ends_with(".monitor"),
            HostKind::Alsa => id.starts_with("alsa:plughw:") && seen_names.insert(name.as_str()),
            HostKind::Native => true,
        };
        if offered && seen_ids.insert(id.as_str()) {
            keep.push(i);
        }
    }
    keep
}

/// What a saved choice resolves to among the offered `(id, name)` candidates.
#[derive(Debug, PartialEq, Eq)]
enum Pick {
    /// The OS default: asked for, or the saved device is not here.
    Default,
    /// The candidate at this index.
    At(usize),
}

fn pick(wanted: Option<&str>, candidates: &[(String, String)], legacy_by_name: bool) -> Pick {
    let Some(wanted) = wanted.map(str::trim).filter(|w| !w.is_empty() && *w != DEFAULT_DEVICE) else {
        return Pick::Default;
    };
    if let Some(i) = candidates.iter().position(|(id, _)| id == wanted) {
        return Pick::At(i);
    }
    let legacy = wanted.parse::<cpal::DeviceId>().is_err();
    if legacy && legacy_by_name {
        if let Some(i) = candidates.iter().position(|(_, name)| name == wanted) {
            return Pick::At(i);
        }
    }
    Pick::Default
}

/// Every device in one direction, before the picker's filtering.
fn direction_devices(host: &cpal::Host, is_input: bool) -> Vec<cpal::Device> {
    let listed = if is_input { host.input_devices() } else { host.output_devices() };
    #[allow(unused_mut)]
    let mut devices: Vec<cpal::Device> = match listed {
        Ok(it) => it.collect(),
        Err(e) => {
            crate::diag!(
                "[voice] {} device enumeration failed: {e}",
                if is_input { "input" } else { "output" }
            );
            Vec::new()
        }
    };
    // macOS: HFP AirPods can be missing from output_devices() while still
    // playing; take any device that advertises output.
    #[cfg(target_os = "macos")]
    if !is_input {
        if let Ok(all) = host.devices() {
            let known: std::collections::HashSet<String> =
                devices.iter().filter_map(|d| d.id().ok()).map(|id| id.to_string()).collect();
            for d in all {
                let Ok(id) = d.id() else { continue };
                if known.contains(&id.to_string()) {
                    continue;
                }
                let plays = d.supported_output_configs().map(|mut c| c.next().is_some()).unwrap_or(false);
                if plays {
                    crate::diag!("[voice] macOS: an output device missing from output_devices(); added via devices()");
                    devices.push(d);
                }
            }
        }
    }
    devices
}

/// One offered device with its id and label.
struct Candidate {
    device: cpal::Device,
    id: String,
    name: String,
}

/// The devices the picker offers in one direction, in host order.
fn candidates(host: &cpal::Host, is_input: bool) -> Vec<Candidate> {
    let mut described: Vec<Candidate> = direction_devices(host, is_input)
        .into_iter()
        .filter_map(|device| {
            let id = device.id().ok()?.to_string();
            let name = device.description().ok()?.name().to_string();
            Some(Candidate { device, id, name })
        })
        .collect();
    let pairs: Vec<(String, String)> = described.iter().map(|c| (c.id.clone(), c.name.clone())).collect();
    let keep = pickable(host_kind(host), &pairs);
    let mut i = 0;
    described.retain(|_| {
        let kept = keep.contains(&i);
        i += 1;
        kept
    });
    described
}

/// The OS default device in one direction.
fn default_device(host: &cpal::Host, is_input: bool) -> Option<cpal::Device> {
    // PulseAudio's per-client default override: libpulse clients honour
    // PULSE_SOURCE / PULSE_SINK, so this client does too. The e2e lab relies on
    // it to pin each test client to its virtual devices (start-audio.sh).
    #[cfg(target_os = "linux")]
    if host_kind(host) == HostKind::Pulse {
        let var = if is_input { "PULSE_SOURCE" } else { "PULSE_SINK" };
        if let Some(name) = std::env::var(var).ok().filter(|n| !n.is_empty()) {
            let wanted = cpal::DeviceId::new(cpal::HostId::PulseAudio, &name);
            let found = direction_devices(host, is_input)
                .into_iter()
                .find(|d| d.id().ok().as_ref() == Some(&wanted));
            if found.is_some() {
                return found;
            }
            crate::diag!("[voice] {var} names no device; using the server default");
        }
    }
    let device = if is_input { host.default_input_device() } else { host.default_output_device() };
    // PulseAudio falls back to a monitor as the default source when there is no
    // microphone; that is the speaker's own output, never a mic.
    #[cfg(target_os = "linux")]
    let device = device.filter(|d| {
        let monitor = is_input && d.id().is_ok_and(|id| id.id().ends_with(".monitor"));
        if monitor {
            crate::diag!("[voice] the default source is a monitor; no microphone");
        }
        !monitor
    });
    // CoreAudio sometimes has no default (Bluetooth handover): use the first.
    #[cfg(target_os = "macos")]
    let device = device.or_else(|| {
        crate::diag!(
            "[voice] macOS default {} device is None; using the first enumerated",
            if is_input { "input" } else { "output" }
        );
        direction_devices(host, is_input).into_iter().next()
    });
    device
}

/// A saved choice resolved to a device on `host`.
struct Resolved {
    device: cpal::Device,
    /// The device's id, when the host reports one.
    id: Option<String>,
    /// True when this is the OS default (asked for, or the fallback).
    is_default: bool,
}

fn resolve(host: &cpal::Host, wanted: Option<&str>, is_input: bool) -> Option<Resolved> {
    let offered = candidates(host, is_input);
    let pairs: Vec<(String, String)> = offered.iter().map(|c| (c.id.clone(), c.name.clone())).collect();
    match pick(wanted, &pairs, LEGACY_NAMES_ARE_DESCRIPTIONS) {
        Pick::At(i) => {
            let c = offered.into_iter().nth(i)?;
            Some(Resolved { device: c.device, id: Some(c.id), is_default: false })
        }
        Pick::Default => {
            let explicit = wanted.is_some_and(|w| !w.trim().is_empty() && w != DEFAULT_DEVICE);
            if explicit {
                crate::diag!(
                    "[voice] saved {} device is not present; using the default",
                    if is_input { "input" } else { "output" }
                );
            }
            let device = default_device(host, is_input)?;
            let id = device.id().ok().map(|id| id.to_string());
            Some(Resolved { device, id, is_default: true })
        }
    }
}

/// Resolve on the shared host; on a PulseAudio host that finds nothing at all,
/// reconnect once (the server restarted under a live connection).
fn resolve_shared(wanted: Option<&str>, is_input: bool) -> (Arc<cpal::Host>, Option<Resolved>) {
    let host = host();
    let found = resolve(&host, wanted, is_input);
    if found.is_some() || host_kind(&host) != HostKind::Pulse {
        return (host, found);
    }
    crate::diag!("[voice] audio server returned no device; reconnecting");
    reset_host();
    let host = self::host();
    let found = resolve(&host, wanted, is_input);
    (host, found)
}

/// A stream (or anything else) opened on a resolved device.
pub(crate) struct Opened<T> {
    pub(crate) value: T,
    /// The id of the device that actually opened (the default's when it fell back).
    pub(crate) device_id: Option<String>,
}

/// Open `wanted` (a saved id, `"default"`/`None`, or a legacy name) with
/// `open`. A choice that names no present device, or whose device refuses to
/// open, falls back to the OS default; only a failing default is an error.
/// Blocking.
pub(crate) fn open_device<T>(
    wanted: Option<&str>,
    is_input: bool,
    open: impl Fn(&cpal::Device) -> Result<T>,
) -> Result<Opened<T>> {
    let kind = if is_input { "input" } else { "output" };
    let (host, resolved) = resolve_shared(wanted, is_input);
    let Some(resolved) = resolved else {
        return Err(anyhow::anyhow!("no audio {kind} device").into());
    };
    match open(&resolved.device) {
        Ok(value) => Ok(Opened { value, device_id: resolved.id }),
        Err(e) if !resolved.is_default => {
            crate::diag!("[voice] {kind} open failed on the saved device ({e}); using the default");
            let device = default_device(&host, is_input)
                .ok_or_else(|| anyhow::anyhow!("no audio {kind} device"))?;
            let device_id = device.id().ok().map(|id| id.to_string());
            Ok(Opened { value: open(&device)?, device_id })
        }
        Err(e) => Err(e),
    }
}

/// The id `wanted` would open right now, without opening anything; `None`
/// when no device exists. Blocking.
pub(crate) fn resolved_id(wanted: Option<&str>, is_input: bool) -> Option<String> {
    resolve_shared(wanted, is_input).1.and_then(|r| r.id)
}

/// Every offered input and output device. Blocking enumeration, so it runs
/// on a blocking thread.
pub async fn list_audio_devices() -> Result<Vec<AudioDevice>> {
    tokio::task::spawn_blocking(|| {
        // Reached from the settings page: bring the hot-plug subscription back
        // if the audio server dropped it.
        super::device_watch::ensure_server_watch();
        let list = |host: &cpal::Host| {
            let mut devices = Vec::new();
            for (is_input, kind) in [(true, "input"), (false, "output")] {
                for c in candidates(host, is_input) {
                    devices.push(AudioDevice { id: c.id, name: c.name, kind: kind.into() });
                }
            }
            devices
        };
        let host = host();
        let devices = list(&host);
        if !devices.is_empty() || host_kind(&host) != HostKind::Pulse {
            return devices;
        }
        crate::diag!("[voice] audio server listed no device; reconnecting");
        reset_host();
        list(&self::host())
    })
    .await
    .map_err(|e| anyhow::anyhow!("device enumeration panicked: {e}").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(id, name)| (id.to_string(), name.to_string())).collect()
    }

    fn kept<'a>(kind: HostKind, pairs: &'a [(&'a str, &'a str)]) -> Vec<&'a str> {
        pickable(kind, &entries(pairs)).into_iter().map(|i| pairs[i].1).collect()
    }

    // `pactl list short sources` on a PipeWire desktop: monitors are speakers' taps.
    #[test]
    fn pulse_lists_every_source_but_monitors() {
        let sources = [
            ("pulseaudio:alsa_output.pci-0000_00_1f.3.analog-stereo.monitor", "Monitor of Built-in Audio Analog Stereo"),
            ("pulseaudio:alsa_input.pci-0000_00_1f.3.analog-stereo", "Built-in Audio Analog Stereo"),
            ("pulseaudio:alsa_input.usb-Blue_Microphones_Yeti-00.analog-stereo", "Yeti Stereo Microphone"),
            ("pulseaudio:bluez_input.AA_BB_CC_DD_EE_FF.0", "AirPods Pro"),
            ("pulseaudio:bluez_output.AA_BB_CC_DD_EE_FF.1.monitor", "Monitor of AirPods Pro"),
        ];
        assert_eq!(
            kept(HostKind::Pulse, &sources),
            ["Built-in Audio Analog Stereo", "Yeti Stereo Microphone", "AirPods Pro"]
        );
    }

    #[test]
    fn alsa_offers_one_plughw_per_physical_device_and_no_plugins() {
        let pcms = [
            ("alsa:null", "Discard all samples (playback) or generate zero samples (capture)"),
            ("alsa:pipewire", "PipeWire Sound Server"),
            ("alsa:pulse", "PulseAudio Sound Server"),
            ("alsa:jack", "JACK Audio Connection Kit"),
            ("alsa:default", "Default ALSA Output (currently PipeWire Media Server)"),
            ("alsa:sysdefault:CARD=PCH", "HDA Intel PCH, ALC892 Analog"),
            ("alsa:surround51:CARD=PCH,DEV=0", "HDA Intel PCH, ALC892 Analog"),
            ("alsa:hw:CARD=0,DEV=0", "HDA Intel PCH, ALC892 Analog"),
            ("alsa:plughw:CARD=0,DEV=0", "HDA Intel PCH, ALC892 Analog"),
            ("alsa:plughw:CARD=1,DEV=0", "Yeti Stereo Microphone, USB Audio"),
            ("alsa:plughw:CARD=Yeti,DEV=0", "Yeti Stereo Microphone, USB Audio"),
        ];
        assert_eq!(
            kept(HostKind::Alsa, &pcms),
            ["HDA Intel PCH, ALC892 Analog", "Yeti Stereo Microphone, USB Audio"]
        );
    }

    #[test]
    fn native_hosts_offer_everything_once() {
        let endpoints = [
            ("coreaudio:BuiltInMicrophoneDevice", "MacBook Pro Microphone"),
            ("coreaudio:AA-BB:input", "AirPods Pro"),
            ("coreaudio:AA-BB:input", "AirPods Pro"),
        ];
        assert_eq!(kept(HostKind::Native, &endpoints), ["MacBook Pro Microphone", "AirPods Pro"]);
    }

    fn pulse_inputs() -> Vec<(String, String)> {
        entries(&[
            ("pulseaudio:alsa_input.pci-0000_00_1f.3.analog-stereo", "Built-in Audio Analog Stereo"),
            ("pulseaudio:alsa_input.usb-Blue_Microphones_Yeti-00.analog-stereo", "Yeti Stereo Microphone"),
        ])
    }

    #[test]
    fn the_default_sentinel_and_no_value_mean_the_default() {
        for wanted in [None, Some(""), Some("  "), Some("default")] {
            assert_eq!(pick(wanted, &pulse_inputs(), false), Pick::Default, "{wanted:?}");
        }
    }

    #[test]
    fn a_saved_id_opens_that_device() {
        let wanted = Some("pulseaudio:alsa_input.usb-Blue_Microphones_Yeti-00.analog-stereo");
        assert_eq!(pick(wanted, &pulse_inputs(), false), Pick::At(1));
    }

    #[test]
    fn a_saved_id_whose_device_is_gone_falls_back_to_the_default() {
        let wanted = Some("pulseaudio:bluez_input.AA_BB_CC_DD_EE_FF.0");
        assert_eq!(pick(wanted, &pulse_inputs(), false), Pick::Default);
        assert_eq!(pick(wanted, &pulse_inputs(), true), Pick::Default);
    }

    // Every value the cpal 0.15 ALSA picker could have saved on Linux.
    #[test]
    fn legacy_linux_alsa_names_fall_back_to_the_default() {
        let legacy = ["pulse", "pipewire", "jack", "default", "hw:CARD=QuadCast,DEV=0", "hw:CARD=0,DEV=0"];
        for wanted in legacy {
            assert_eq!(pick(Some(wanted), &pulse_inputs(), LEGACY_LINUX), Pick::Default, "{wanted}");
        }
        // Even if some device happened to carry that text as its description.
        let odd = entries(&[("pulseaudio:x", "pulse")]);
        assert_eq!(pick(Some("pulse"), &odd, LEGACY_LINUX), Pick::Default);
    }

    const LEGACY_LINUX: bool = false;
    const LEGACY_NATIVE: bool = true;

    // cpal 0.15 saved CoreAudio/WASAPI names, which are 0.18's descriptions.
    #[test]
    fn legacy_macos_and_windows_names_keep_their_device() {
        let mac = entries(&[
            ("coreaudio:BuiltInMicrophoneDevice", "MacBook Pro Microphone"),
            ("coreaudio:AA-BB-CC:input", "Dan's AirPods Pro"),
        ]);
        assert_eq!(pick(Some("Dan's AirPods Pro"), &mac, LEGACY_NATIVE), Pick::At(1));
        let win = entries(&[
            ("wasapi:{0.0.1.00000000}.{aaaa}", "Microphone (Realtek(R) Audio)"),
            ("wasapi:{0.0.1.00000000}.{bbbb}", "Headset Microphone (2- Jabra EVOLVE 30 II)"),
        ]);
        assert_eq!(pick(Some("Headset Microphone (2- Jabra EVOLVE 30 II)"), &win, LEGACY_NATIVE), Pick::At(1));
        // A legacy name no device carries any more: the default, never an error.
        assert_eq!(pick(Some("USB Audio CODEC"), &mac, LEGACY_NATIVE), Pick::Default);
    }

    #[test]
    fn garbage_falls_back_to_the_default() {
        for wanted in ["{\"input\":1}", "\u{0}", "coreaudio:", "nonsense:host:id"] {
            assert_eq!(pick(Some(wanted), &pulse_inputs(), LEGACY_NATIVE), Pick::Default, "{wanted:?}");
        }
    }

    // `pulseaudio:` parses as an id only where that host is compiled in.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_id_never_matches_by_name() {
        // Parses as an id, so it is not a legacy name even though one device's label equals it.
        let devices = entries(&[("pulseaudio:a", "pulseaudio:b")]);
        assert_eq!(pick(Some("pulseaudio:b"), &devices, LEGACY_NATIVE), Pick::Default);
    }

    /// Prints what the picker would show on this machine; compare with
    /// `pactl list short sources` / `sinks`. Opens no stream.
    /// `cargo test -p pollis-core --lib devices::tests::print_this_machines_devices -- --ignored --nocapture`
    #[test]
    #[ignore = "enumerates this machine's real audio devices"]
    fn print_this_machines_devices() {
        let host = host();
        println!("host: {}", host.id());
        for is_input in [true, false] {
            println!("{}:", if is_input { "inputs" } else { "outputs" });
            for c in candidates(&host, is_input) {
                println!("  {}  [{}]", c.name, c.id);
            }
            println!("  default -> {:?}", resolved_id(None, is_input));
        }
    }
}
