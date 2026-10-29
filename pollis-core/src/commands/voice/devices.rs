use cpal::traits::{DeviceTrait, HostTrait};

use crate::error::Result;

use super::types::AudioDevice;

pub(crate) fn get_device(host: &cpal::Host, name: Option<&str>, is_input: bool) -> Result<cpal::Device> {
    // "default" or "" from the frontend means the OS default, not a device
    // literally named "default".
    let name = name.filter(|n| !n.is_empty() && *n != "default");
    let device = match name {
        None => {
            // On Linux, ALSA's default may be a virtual device like "vdownmix"
            // that crashes when opened for capture: try the audio-server PCMs,
            // then the first useful device, else fail rather than open a
            // blocked one.
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
                let devices: Vec<cpal::Device> = iter
                    .filter(|d| {
                        d.name()
                            .ok()
                            .map(|n| !blocked.contains(&n.as_str()))
                            .unwrap_or(true)
                    })
                    .collect();

                crate::diag!("[voice] available {} devices (blocked filtered): {}",
                    if is_input { "input" } else { "output" }, devices.len());

                let found = preferred.iter().find_map(|&pref| {
                    devices.iter().position(|d| d.name().ok().as_deref() == Some(pref))
                });
                if let Some(idx) = found {
                    devices.into_iter().nth(idx)
                } else {
                    // e.g. hw:CARD=...,DEV=0
                    devices
                        .into_iter()
                        .find(|d| d.name().ok().map(|n| is_useful_device(&n)).unwrap_or(false))
                }
            }
            #[cfg(target_os = "macos")]
            {
                // CoreAudio sometimes reports no default (mid Bluetooth
                // handover, or a stale default): use the first device.
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
            // Blocked devices are rejected even when named; auto-detect, so a
            // stale preference cannot crash the app.
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
            // AirPods and other Bluetooth duplex devices in HFP/SCO can drop out
            // of `output_devices()` while their mic is captured; search all
            // scopes and validate.
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
            // Last resort: the first device supporting the scope (a duplex
            // device in HFP can be missing from the scoped list).
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
    // Several AudioObjects can share a name (aggregates, BlackHole), and a
    // stub can hang in default_*_config(), so direction-validate each match.
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
    // AirPods in HFP can appear only in the opposite scope.
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

/// Linux allowlist: well-known audio servers and direct hardware (hw:...,
/// DEV=0); sysdefault, speex, upmix, front:, iec958: and the rest are
/// filtered. Everything passes on macOS and Windows.
fn is_useful_device(_name: &str) -> bool {
    #[cfg(not(target_os = "linux"))]
    return true;
    #[cfg(target_os = "linux")]
    {
        matches!(_name, "default" | "pulse" | "pipewire" | "jack")
            || (_name.starts_with("hw:") && _name.contains("DEV=0"))
    }
}

/// A human-readable label: "hw:CARD=QuadCast,DEV=0" → "QuadCast",
/// "pipewire" → "PipeWire".
fn display_name(raw: &str) -> String {
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

/// Every available audio input and output device. Enumeration makes blocking
/// ALSA syscalls, so it runs on a blocking thread.
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
        // macOS: surface duplex devices dropped from output_devices() in HFP.
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
