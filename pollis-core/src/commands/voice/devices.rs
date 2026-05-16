use cpal::traits::{DeviceTrait, HostTrait};

use crate::error::Result;

use super::types::AudioDevice;

pub(crate) fn get_device(host: &cpal::Host, name: Option<&str>, is_input: bool) -> Result<cpal::Device> {
    // Frontend sends "default" (or "") to mean "use the OS default" rather
    // than a real device id. Treat those as None so we don't go looking
    // for a device literally named "default".
    let name = name.filter(|n| !n.is_empty() && *n != "default");
    let device = match name {
        None => {
            // On Linux, ALSA's system default may be a virtual device like
            // "vdownmix" (surround downmix) that crashes when opened for capture.
            // Strategy:
            //   1. Try well-known audio-server PCMs: pipewire, pulse, default.
            //   2. Fall back to first device that passes is_useful_device.
            //   3. Return error rather than opening a blocked device.
            #[cfg(target_os = "linux")]
            {
                // Virtual ALSA devices known to crash or produce no audio.
                let blocked: &[&str] = &["vdownmix", "upmix", "speex", "speexrate"];
                let preferred: &[&str] = &["pipewire", "pulse", "default"];

                let iter = if is_input {
                    host.input_devices().map_err(|e| anyhow::anyhow!("enumerate devices: {e}"))?
                } else {
                    host.output_devices().map_err(|e| anyhow::anyhow!("enumerate devices: {e}"))?
                };
                // Collect, filter blocked names, log what we see.
                let devices: Vec<cpal::Device> = iter
                    .filter(|d| {
                        d.name()
                            .ok()
                            .map(|n| !blocked.contains(&n.as_str()))
                            .unwrap_or(true)
                    })
                    .collect();

                let names: Vec<String> = devices.iter().filter_map(|d| d.name().ok()).collect();
                eprintln!("[voice] available {} devices (blocked filtered): {:?}",
                    if is_input { "input" } else { "output" }, names);

                // 1. Preferred by name
                let found = preferred.iter().find_map(|&pref| {
                    devices.iter().position(|d| d.name().ok().as_deref() == Some(pref))
                });
                if let Some(idx) = found {
                    devices.into_iter().nth(idx)
                } else {
                    // 2. First device that passes the useful filter (e.g. hw:CARD=...,DEV=0)
                    devices
                        .into_iter()
                        .find(|d| d.name().ok().map(|n| is_useful_device(&n)).unwrap_or(false))
                }
            }
            #[cfg(target_os = "macos")]
            {
                // CoreAudio occasionally reports no default device (e.g. mid
                // Bluetooth handover, or a stale default after a device went
                // away). Fall back to the first enumerated device so a missing
                // default doesn't break voice.
                let default_dev = if is_input {
                    host.default_input_device()
                } else {
                    host.default_output_device()
                };
                if default_dev.is_none() {
                    eprintln!(
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
            // On Linux, reject blocked devices even when explicitly named.
            // Fall back to auto-detect so a stale preference doesn't crash the app.
            #[cfg(target_os = "linux")]
            {
                let blocked: &[&str] = &["vdownmix", "upmix", "speex", "speexrate"];
                if blocked.contains(&n) {
                    eprintln!("[voice] device '{n}' is blocked on Linux — auto-selecting");
                    return get_device(host, None, is_input);
                }
            }
            let iter = if is_input {
                host.input_devices().map_err(|e| anyhow::anyhow!("enumerate devices: {e}"))?
            } else {
                host.output_devices().map_err(|e| anyhow::anyhow!("enumerate devices: {e}"))?
            };
            let found = iter.filter(|d| d.name().ok().as_deref() == Some(n)).next();
            // macOS-only fallback: AirPods (and other Bluetooth duplex devices)
            // in HFP/SCO mode sometimes drop out of host.output_devices() while
            // their mic is captured, even though the device DOES support output.
            // Try host.devices() (all scopes) and validate via supported_output_configs.
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
            // Last-resort: scan all devices and pick the first that supports
            // the requested scope. Required when a duplex device (AirPods in
            // HFP mode) is missing from the scoped list but present in devices().
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
    // Multiple AudioObjects can share a name (system-level objects, aggregate
    // devices, BlackHole virtual devices). Returning the first name-match
    // unconditionally can hand back a stub Device that hangs inside
    // default_input_config() / default_output_config(). Direction-validate
    // each candidate via supported_*_configs() and skip anything that
    // doesn't report a usable stream for the requested scope.
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
                eprintln!(
                    "[voice] macOS: found '{name}' via host.devices() fallback ({} validated)",
                    if is_input { "input" } else { "output" }
                );
                return Some(d);
            }
        }
    }
    // Opposite-scope last resort: AirPods in HFP can appear only in the input
    // list while still capable of output (or vice versa). Still direction-
    // validate so we don't hand back a stub.
    let opposite = if is_input { host.output_devices().ok() } else { host.input_devices().ok() };
    if let Some(it) = opposite {
        for d in it {
            if d.name().ok().as_deref() == Some(name) && supports(&d) {
                eprintln!(
                    "[voice] macOS: found '{name}' via opposite-scope enumeration ({} validated)",
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
    eprintln!(
        "[voice] macOS get_device failed — wanted={wanted:?} kind={kind} scoped={scoped:?} all={all:?}"
    );
}

// ── Device name helpers ───────────────────────────────────────────────────

/// Allowlist for Linux: only keep well-known virtual devices and direct
/// hardware interfaces (hw:CARD=X,DEV=0). Everything else — sysdefault,
/// speex, upmix, vdownmix, front:, iec958:, etc. — is filtered out.
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
/// Device enumeration makes blocking ALSA syscalls (and produces ALSA warning
/// spam); run it on a blocking thread to avoid stalling the tokio runtime.
pub async fn list_audio_devices() -> Result<Vec<AudioDevice>> {
    tokio::task::spawn_blocking(|| {
        let host = cpal::default_host();
        let mut devices = Vec::new();

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
            Err(e) => eprintln!("[voice] list_audio_devices: input enumeration failed: {e}"),
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
            Err(e) => eprintln!("[voice] list_audio_devices: output enumeration failed: {e}"),
        }
        // On macOS, a duplex device (AirPods, USB headset) in HFP mode can
        // get dropped from output_devices() while still listed by devices().
        // Surface those as output options so the user can still pick them.
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
                    eprintln!("[voice] macOS: '{name}' missing from output_devices() — adding via devices() fallback");
                    devices.push(AudioDevice { id: name.clone(), name: display_name(&name), kind: "output".into() });
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            let inputs: Vec<&str> = devices.iter().filter(|d| d.kind == "input").map(|d| d.id.as_str()).collect();
            let outputs: Vec<&str> = devices.iter().filter(|d| d.kind == "output").map(|d| d.id.as_str()).collect();
            eprintln!("[voice] macOS list_audio_devices — inputs={inputs:?} outputs={outputs:?}");
        }
        devices
    })
    .await
    .map_err(|e| anyhow::anyhow!("device enumeration panicked: {e}").into())
}
