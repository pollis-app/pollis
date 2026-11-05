use cpal::traits::{DeviceTrait, HostTrait};

use crate::error::Result;

use super::types::AudioDevice;

/// Virtual ALSA devices known to crash or produce no audio.
#[cfg(target_os = "linux")]
const BLOCKED_ALSA: &[&str] = &["vdownmix", "upmix", "speex", "speexrate"];

/// The ALSA PCMs cpal's `get_device(None)` picks from, best first. On a PipeWire box
/// `pipewire` is the first hint that opens, so a scan that stops at it opens two PCMs,
/// not every HDMI/USB one after it.
#[cfg(target_os = "linux")]
const PREFERRED_ALSA: &[&str] = &["pipewire", "pulse", "default"];

/// cpal's ALSA enumeration OPENS every PCM it lists (hundreds of ms, seconds on a
/// busy PipeWire box), and the join did that on every mic open. What is kept is only
/// the NAMES (a `cpal::Device` holds open PCM handles, and idle PipeWire handles cost
/// threads and memory); with the names known, the join scans lazily and stops at the
/// best preferred PCM present. Refreshed by the warmup and `list_audio_devices`.
#[cfg(target_os = "linux")]
mod enum_cache {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    pub struct Names {
        pub at: Instant,
        pub input: Vec<String>,
        pub output: Vec<String>,
    }

    pub static CACHE: Mutex<Option<Names>> = Mutex::new(None);
    pub const TTL: Duration = Duration::from_secs(300);
}

/// The cached names for one direction, if fresh.
#[cfg(target_os = "linux")]
fn cached_names(is_input: bool) -> Option<Vec<String>> {
    let cache = enum_cache::CACHE.lock().unwrap_or_else(|p| p.into_inner());
    cache
        .as_ref()
        .filter(|n| n.at.elapsed() < enum_cache::TTL)
        .map(|n| if is_input { n.input.clone() } else { n.output.clone() })
}

/// Full enumeration (every PCM opened and dropped again); only the names are kept.
#[cfg(target_os = "linux")]
fn refresh_names(host: &cpal::Host) -> Result<()> {
    let started = std::time::Instant::now();
    fn names(it: impl Iterator<Item = cpal::Device>) -> Vec<String> {
        it.filter_map(|d| d.name().ok())
            .filter(|n| !BLOCKED_ALSA.contains(&n.as_str()))
            .collect()
    }
    let input = names(host.input_devices().map_err(|e| anyhow::anyhow!("enumerate devices: {e}"))?);
    let output = names(host.output_devices().map_err(|e| anyhow::anyhow!("enumerate devices: {e}"))?);
    crate::diag!(
        "[voice] enumerated {} input / {} output devices in {:.0}ms (blocked filtered)",
        input.len(),
        output.len(),
        started.elapsed().as_secs_f64() * 1000.0
    );
    *enum_cache::CACHE.lock().unwrap_or_else(|p| p.into_inner()) =
        Some(enum_cache::Names { at: std::time::Instant::now(), input, output });
    Ok(())
}

/// Enumerate on a blocking thread now so the next join knows which PCM to stop at.
pub(crate) fn warm_device_cache() {
    #[cfg(target_os = "linux")]
    {
        let fresh = {
            let cache = enum_cache::CACHE.lock().unwrap_or_else(|p| p.into_inner());
            cache.as_ref().is_some_and(|n| n.at.elapsed() < enum_cache::TTL)
        };
        if fresh {
            return;
        }
        tokio::task::spawn_blocking(|| {
            let host = cpal::default_host();
            let _ = refresh_names(&host);
        });
    }
}

/// The best preferred PCM a scan should stop at, or `None` when the names are unknown
/// (then the scan stops at the first preferred one it meets).
#[cfg(target_os = "linux")]
fn preferred_target(names: &[String]) -> Option<&'static str> {
    PREFERRED_ALSA.iter().copied().find(|p| names.iter().any(|n| n == p))
}

/// Walk the hints in order, opening as few PCMs as possible: stop at `target` (or
/// the first preferred name when none is known); else the first useful device.
#[cfg(target_os = "linux")]
fn pick_lazily(iter: impl Iterator<Item = cpal::Device>, target: Option<&str>) -> Option<cpal::Device> {
    let mut first_useful: Option<cpal::Device> = None;
    for d in iter {
        let Ok(name) = d.name() else { continue };
        if BLOCKED_ALSA.contains(&name.as_str()) {
            continue;
        }
        let hit = match target {
            Some(t) => name == t,
            None => PREFERRED_ALSA.contains(&name.as_str()),
        };
        if hit {
            return Some(d);
        }
        if first_useful.is_none() && is_useful_device(&name) {
            first_useful = Some(d);
        }
    }
    first_useful
}

pub(crate) fn get_device(host: &cpal::Host, name: Option<&str>, is_input: bool) -> Result<cpal::Device> {
    // "default"/"" mean the OS default, not a device named "default".
    let name = name.filter(|n| !n.is_empty() && *n != "default");
    let device = match name {
        None => {
            // Linux: ALSA's default may be a virtual device (vdownmix) that crashes.
            #[cfg(target_os = "linux")]
            {
                let names = cached_names(is_input);
                let target = names.as_deref().and_then(preferred_target);
                let iter = if is_input {
                    host.input_devices().map_err(|e| anyhow::anyhow!("enumerate devices: {e}"))?
                } else {
                    host.output_devices().map_err(|e| anyhow::anyhow!("enumerate devices: {e}"))?
                };
                pick_lazily(iter, target)
            }
            #[cfg(target_os = "macos")]
            {
                // CoreAudio sometimes has no default (Bluetooth handover): use the first.
                let default_dev = if is_input {
                    host.default_input_device()
                } else {
                    host.default_output_device()
                };
                if default_dev.is_none() {
                    crate::diag!(
                        "[voice] macOS default {} device is None — falling back to first enumerated",
                        if is_input { "input" } else { "output" }
                    );
                }
                default_dev.or_else(|| macos_first_device(host, is_input))
            }
            #[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
            {
                if is_input { host.default_input_device() } else { host.default_output_device() }
            }
        }
        Some(n) => {
            // Linux: blocked even when named; auto-detect instead of crashing.
            #[cfg(target_os = "linux")]
            {
                let blocked: &[&str] = &["vdownmix", "upmix", "speex", "speexrate"];
                if blocked.contains(&n) {
                    crate::diag!("[voice] device '{n}' is blocked on Linux — auto-selecting");
                    return get_device(host, None, is_input);
                }
            }
            let mut iter = if is_input {
                host.input_devices().map_err(|e| anyhow::anyhow!("enumerate devices: {e}"))?
            } else {
                host.output_devices().map_err(|e| anyhow::anyhow!("enumerate devices: {e}"))?
            };
            let found = iter.find(|d| d.name().ok().as_deref() == Some(n));
            // macOS: HFP AirPods can vanish from output_devices(); try all scopes.
            #[cfg(target_os = "macos")]
            let found = found.or_else(|| macos_lookup_any_scope(host, n, is_input));
            found
        }
    };
    if device.is_none() {
        #[cfg(target_os = "macos")]
        macos_log_enumeration(host, name, is_input);
    }
    device.ok_or_else(|| anyhow::anyhow!("audio device not found").into())
}

#[cfg(target_os = "macos")]
fn macos_first_device(host: &cpal::Host, is_input: bool) -> Option<cpal::Device> {
    let iter = if is_input { host.input_devices().ok() } else { host.output_devices().ok() };
    iter.and_then(|mut it| it.next())
        .or_else(|| {
            // Last resort: first device in devices() supporting this scope.
            host.devices().ok().and_then(|all| {
                all.filter(|d| {
                    if is_input {
                        d.supported_input_configs().map(|mut c| c.next().is_some()).unwrap_or(false)
                    } else {
                        d.supported_output_configs().map(|mut c| c.next().is_some()).unwrap_or(false)
                    }
                })
                .next()
            })
        })
}

#[cfg(target_os = "macos")]
fn macos_lookup_any_scope(host: &cpal::Host, name: &str, is_input: bool) -> Option<cpal::Device> {
    // Names collide (aggregate/BlackHole stubs that hang); direction-validate each.
    let supports = |d: &cpal::Device| -> bool {
        if is_input {
            d.supported_input_configs().map(|mut c| c.next().is_some()).unwrap_or(false)
        } else {
            d.supported_output_configs().map(|mut c| c.next().is_some()).unwrap_or(false)
        }
    };
    if let Ok(all) = host.devices() {
        for d in all {
            if d.name().ok().as_deref() == Some(name) && supports(&d) {
                crate::diag!(
                    "[voice] macOS: found the device via host.devices() fallback ({} validated)",
                    if is_input { "input" } else { "output" }
                );
                return Some(d);
            }
        }
    }
    // Opposite-scope last resort (HFP AirPods), still direction-validated.
    let opposite = if is_input { host.output_devices().ok() } else { host.input_devices().ok() };
    if let Some(it) = opposite {
        for d in it {
            if d.name().ok().as_deref() == Some(name) && supports(&d) {
                crate::diag!(
                    "[voice] macOS: found the device via opposite-scope enumeration ({} validated)",
                    if is_input { "input" } else { "output" }
                );
                return Some(d);
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn macos_log_enumeration(host: &cpal::Host, wanted: Option<&str>, is_input: bool) {
    let kind = if is_input { "input" } else { "output" };
    let scoped: Vec<String> = if is_input {
        host.input_devices().ok().map(|it| it.filter_map(|d| d.name().ok()).collect()).unwrap_or_default()
    } else {
        host.output_devices().ok().map(|it| it.filter_map(|d| d.name().ok()).collect()).unwrap_or_default()
    };
    let all: Vec<String> = host.devices().ok()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default();
    crate::diag!(
        "[voice] macOS get_device failed — wanted={wanted:?} kind={kind} scoped={scoped:?} all={all:?}"
    );
}

/// Linux allowlist: known audio servers and hw:CARD=X,DEV=0 only.
/// On macOS/Windows all devices pass through.
fn is_useful_device(_name: &str) -> bool {
    #[cfg(not(target_os = "linux"))]
    return true;
    #[cfg(target_os = "linux")]
    {
        matches!(_name, "default" | "pulse" | "pipewire" | "jack")
            || (_name.starts_with("hw:") && _name.contains("DEV=0"))
    }
}

/// Returns a human-readable label.
/// "hw:CARD=QuadCast,DEV=0" → "QuadCast"
/// "pipewire" → "PipeWire"
fn display_name(raw: &str) -> String {
    // On Linux, extract the card name from hw:CARD=X,DEV=0
    #[cfg(target_os = "linux")]
    if raw.starts_with("hw:") {
        if let Some(card_part) = raw.split(',').next() {
            if let Some((_, card_name)) = card_part.split_once("CARD=") {
                return card_name.to_string();
            }
        }
    }
    match raw {
        "default" => "System Default".to_string(),
        "pulse" => "PulseAudio".to_string(),
        "pipewire" => "PipeWire".to_string(),
        "jack" => "JACK".to_string(),
        other => other.to_string(),
    }
}

/// Return all available audio input and output devices.
/// Enumeration blocks (ALSA), so it runs on a blocking thread.
pub async fn list_audio_devices() -> Result<Vec<AudioDevice>> {
    tokio::task::spawn_blocking(|| {
        let host = cpal::default_host();
        let mut devices = Vec::new();
        // The settings page is where a newly plugged device is looked for: refresh the join's name cache too.
        #[cfg(target_os = "linux")]
        let _ = refresh_names(&host);

        match host.input_devices() {
            Ok(inputs) => {
                for d in inputs {
                    if let Ok(name) = d.name() {
                        if is_useful_device(&name) {
                            devices.push(AudioDevice { id: name.clone(), name: display_name(&name), kind: "input".into() });
                        }
                    }
                }
            }
            Err(e) => crate::diag!("[voice] list_audio_devices: input enumeration failed: {e}"),
        }
        match host.output_devices() {
            Ok(outputs) => {
                for d in outputs {
                    if let Ok(name) = d.name() {
                        if is_useful_device(&name) {
                            devices.push(AudioDevice { id: name.clone(), name: display_name(&name), kind: "output".into() });
                        }
                    }
                }
            }
            Err(e) => crate::diag!("[voice] list_audio_devices: output enumeration failed: {e}"),
        }
        // macOS: surface HFP duplex devices missing from output_devices().
        #[cfg(target_os = "macos")]
        if let Ok(all) = host.devices() {
            let seen_output: std::collections::HashSet<String> = devices
                .iter()
                .filter(|d| d.kind == "output")
                .map(|d| d.id.clone())
                .collect();
            for d in all {
                let Ok(name) = d.name() else { continue };
                if seen_output.contains(&name) { continue; }
                let supports_output = d
                    .supported_output_configs()
                    .map(|mut c| c.next().is_some())
                    .unwrap_or(false);
                if supports_output {
                    crate::diag!("[voice] macOS: an output device missing from output_devices() — adding via devices() fallback");
                    devices.push(AudioDevice { id: name.clone(), name: display_name(&name), kind: "output".into() });
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            let inputs: Vec<&str> = devices.iter().filter(|d| d.kind == "input").map(|d| d.id.as_str()).collect();
            let outputs: Vec<&str> = devices.iter().filter(|d| d.kind == "output").map(|d| d.id.as_str()).collect();
            crate::diag!("[voice] macOS list_audio_devices — inputs={inputs:?} outputs={outputs:?}");
        }
        devices
    })
    .await
    .map_err(|e| anyhow::anyhow!("device enumeration panicked: {e}").into())
}
