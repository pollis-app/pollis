use cpal::traits::{DeviceTrait, HostTrait};

use crate::error::Result;

use super::types::AudioDevice;

pub(crate) fn get_device(host: &cpal::Host, name: Option<&str>, is_input: bool) -> Result<cpal::Device> {
    // "default"/"" mean the OS default, not a device named "default".
    let name = name.filter(|n| !n.is_empty() && *n != "default");
    let device = match name {
        None => {
            // Linux: ALSA's default may be a virtual device (vdownmix) that crashes.
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
                    // Else the first useful device (e.g. hw:CARD=...,DEV=0)
                    devices
                        .into_iter()
                        .find(|d| d.name().ok().map(|n| is_useful_device(&n)).unwrap_or(false))
                }
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
