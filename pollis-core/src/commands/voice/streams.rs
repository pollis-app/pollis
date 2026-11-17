use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use cpal::traits::{DeviceTrait, StreamTrait};

use crate::error::Result;

use super::resampler::MIX_RATE_HZ;
use super::types::SendableStream;

/// Buffer requests to try, best first. The numbers come from cpal's host
/// mapping:
/// - PulseAudio (`pulse`, #1989): a record stream delivers `Fixed(v)`-frame
///   fragments, so the mic asks for `rate/100` (10 ms). A playback stream
///   requests `v` frames per callback against a `2v` target, so the speaker
///   asks for `rate/50`: 20 ms callbacks under the same 40 ms the ALSA path
///   buffers.
/// - ALSA takes `Fixed(v)` as a `v`-frame buffer with `v/4` periods, so
///   `rate/25` is a 10 ms period under a 40 ms buffer (PipeWire's ALSA plugin
///   accepts it; measured 10.7 ms callbacks against 42.7 ms with `Default`).
/// - CoreAudio sets its period to `v` directly; WASAPI shared mode takes `v`
///   as the buffer over its fixed 10 ms engine period.
fn buffer_candidates(rate: u32, pulse: bool, is_input: bool) -> [cpal::BufferSize; 2] {
    #[cfg(target_os = "linux")]
    let fixed = match (pulse, is_input) {
        (true, true) => rate / 100,
        (true, false) => rate / 50,
        (false, _) => rate / 25,
    };
    #[cfg(target_os = "macos")]
    let fixed = rate / 100;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let fixed = rate / 50;
    #[cfg(not(target_os = "linux"))]
    let _ = (pulse, is_input);
    [cpal::BufferSize::Fixed(fixed), cpal::BufferSize::Default]
}

/// Whether `device` is a PulseAudio sink/source (Linux with a sound server).
fn is_pulse(device: &cpal::Device) -> bool {
    #[cfg(target_os = "linux")]
    {
        matches!(device.as_inner(), cpal::platform::DeviceInner::PulseAudio(_))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = device;
        false
    }
}

/// Formats the openers convert from/to.
const HANDLED_FORMATS: [cpal::SampleFormat; 5] = [
    cpal::SampleFormat::F32,
    cpal::SampleFormat::I16,
    cpal::SampleFormat::U16,
    cpal::SampleFormat::I32,
    cpal::SampleFormat::U32,
];

/// The sample format and rate to open at. The device's default format when
/// the openers handle it, else f32 if the device takes it (a PulseAudio source
/// whose hardware is S24 reports I24 as its default, and the server converts
/// to anything). Then 48 kHz (`preferred`) if the device advertises it for
/// that format and channel count, else the device's default rate.
fn pick_config(
    configs: &[cpal::SupportedStreamConfigRange],
    default: &cpal::SupportedStreamConfig,
    preferred: u32,
) -> (cpal::SampleFormat, u32) {
    let channels = default.channels();
    let offers = |format: cpal::SampleFormat| {
        configs.iter().filter(move |c| c.channels() == channels && c.sample_format() == format)
    };
    let format = if HANDLED_FORMATS.contains(&default.sample_format()) {
        default.sample_format()
    } else if offers(cpal::SampleFormat::F32).next().is_some() {
        cpal::SampleFormat::F32
    } else {
        default.sample_format()
    };
    let rate = if offers(format).any(|c| c.min_sample_rate() <= preferred && c.max_sample_rate() >= preferred) {
        preferred
    } else {
        default.sample_rate()
    };
    (format, rate)
}

/// Called from cpal's thread when a stream reports an error; `voice/device_watch.rs`
/// turns a lost device into a reopen.
pub(crate) type StreamErrorHook = Arc<dyn Fn(&cpal::Error) + Send + Sync>;

/// The error callback every stream gets: logs it (a dropped period is not
/// worth a line each) and hands it to `hook`.
fn error_callback(
    what: &'static str,
    hook: Option<StreamErrorHook>,
) -> impl FnMut(cpal::Error) + Send + 'static {
    move |e: cpal::Error| {
        if e.kind() != cpal::ErrorKind::Xrun {
            crate::diag!("[voice] {what} error: {e}");
        }
        if let Some(hook) = &hook {
            hook(&e);
        }
    }
}

/// Open the mic. `sink` runs on the audio thread once per device period with
/// mono i16 at the returned rate, from a scratch buffer reused across calls.
///
/// Never gated here: mute is applied downstream so RNNoise/APM keep their
/// state and the local meter stays alive.
pub(crate) fn build_mic_stream<F>(
    device: &cpal::Device,
    sink: F,
    on_error: Option<StreamErrorHook>,
) -> Result<(SendableStream, u32)>
where
    F: FnMut(&[i16], u32) + Send + 'static,
{
    let config = device
        .default_input_config()
        .map_err(|e| anyhow::anyhow!("input config: {e}"))?;
    let channels = config.channels() as usize;
    let supported: Vec<_> = device.supported_input_configs().map(|c| c.collect()).unwrap_or_default();
    // 48 kHz when the device offers it means no resampling at all; otherwise
    // the native rate (macOS SCO AirPods: 16/24 kHz) and `capture.rs` resamples.
    let (sample_format, sample_rate) = pick_config(&supported, &config, MIX_RATE_HZ);
    let pulse = is_pulse(device);

    let mono: Vec<i16> = Vec::with_capacity(8_192);
    // cpal consumes the callback even when the build fails, so the state is
    // shared across attempts; the lock is only ever taken by the one audio
    // thread of the stream that succeeded.
    let state = Arc::new(Mutex::new((mono, sink)));
    let deliver = move |state: &Mutex<(Vec<i16>, F)>, f32s: &mut dyn Iterator<Item = f32>| {
        let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
        let (mono, sink) = &mut *guard;
        mono.clear();
        if channels == 1 {
            mono.extend(f32s.map(|s| (s * 32_767.0).clamp(-32_768.0, 32_767.0) as i16));
        } else {
            let inv = 1.0 / channels as f32;
            let mut acc = 0.0f32;
            let mut n = 0usize;
            for s in f32s {
                acc += s;
                n += 1;
                if n == channels {
                    mono.push((acc * inv * 32_767.0).clamp(-32_768.0, 32_767.0) as i16);
                    acc = 0.0;
                    n = 0;
                }
            }
        }
        sink(mono, sample_rate);
    };

    macro_rules! build_input {
        ($T:ty, $to_f32:expr, $cfg:expr) => {{
            let state = Arc::clone(&state);
            device.build_input_stream::<$T, _, _>(
                $cfg,
                move |data: &[$T], _| {
                    deliver(&state, &mut data.iter().copied().map($to_f32));
                },
                error_callback("mic", on_error.clone()),
                None,
            )
        }};
    }

    let mut last_err = None;
    for buffer_size in buffer_candidates(sample_rate, pulse, true) {
        let stream_config = cpal::StreamConfig {
            channels: config.channels(),
            sample_rate,
            buffer_size,
        };
        let built = match sample_format {
            cpal::SampleFormat::F32 => build_input!(f32, |s: f32| s, stream_config),
            cpal::SampleFormat::I16 => {
                build_input!(i16, |s: i16| s as f32 / 32_768.0, stream_config)
            }
            cpal::SampleFormat::U16 => build_input!(
                u16,
                |s: u16| (s as f32 - 32_768.0) / 32_768.0,
                stream_config
            ),
            cpal::SampleFormat::I32 => {
                build_input!(i32, |s: i32| s as f32 / 2_147_483_648.0, stream_config)
            }
            cpal::SampleFormat::U32 => build_input!(
                u32,
                |s: u32| (s as f64 / 2_147_483_648.0 - 1.0) as f32,
                stream_config
            ),
            other => return Err(anyhow::anyhow!("unsupported mic format: {other:?}").into()),
        };
        match built {
            Ok(stream) => {
                stream
                    .play()
                    .map_err(|e| anyhow::anyhow!("mic stream play: {e}"))?;
                crate::diag!("[voice] mic open @ {sample_rate} Hz, buffer {buffer_size:?}");
                return Ok((SendableStream(stream), sample_rate));
            }
            Err(e) => {
                crate::diag!("[voice] mic buffer {buffer_size:?} rejected ({e}); trying the next");
                last_err = Some(e);
            }
        }
    }
    Err(anyhow::anyhow!(
        "build mic stream: {}",
        last_err.map(|e| e.to_string()).unwrap_or_default()
    )
    .into())
}

/// Mic for the voice-test harness: 10 ms-ish chunks over an mpsc, gated by
/// `is_muted` (the explicit mute only; the mic test ignores PTT).
pub(crate) fn start_mic_stream(
    device: &cpal::Device,
    frame_tx: tokio::sync::mpsc::UnboundedSender<(Vec<i16>, u32)>,
    is_muted: Arc<AtomicBool>,
) -> Result<(SendableStream, u32)> {
    build_mic_stream(
        device,
        move |mono, rate| {
            if is_muted.load(Ordering::Relaxed) {
                return;
            }
            let _ = frame_tx.send((mono.to_vec(), rate));
        },
        None,
    )
}

/// Open the speaker. `make_fill(rate, channels)` builds the callback that
/// fills each interleaved f32 period; it runs on the audio thread.
///
/// Honours 48 kHz if the device offers it, else the device default rate.
pub(crate) fn build_output_stream<F, M>(
    device: &cpal::Device,
    make_fill: M,
    on_error: Option<StreamErrorHook>,
) -> Result<(SendableStream, u32, u32)>
where
    F: FnMut(&mut [f32]) + Send + 'static,
    M: FnOnce(u32, u32) -> F,
{
    let config = device
        .default_output_config()
        .map_err(|e| anyhow::anyhow!("output config: {e}"))?;
    let channels = config.channels() as u32;
    let supported: Vec<_> = device.supported_output_configs().map(|c| c.collect()).unwrap_or_default();
    let (sample_format, sample_rate) = pick_config(&supported, &config, MIX_RATE_HZ);
    let pulse = is_pulse(device);

    let fill = make_fill(sample_rate, channels);
    // Converted formats render into this and copy out; sized for a 40 ms
    // period so steady state never allocates.
    let scratch: Vec<f32> = vec![0.0; (sample_rate as usize / 25) * channels as usize];
    // Shared across build attempts for the same reason as the mic's state.
    let state = Arc::new(Mutex::new((scratch, fill)));

    macro_rules! build_output {
        ($T:ty, $from_f32:expr, $cfg:expr) => {{
            let state = Arc::clone(&state);
            device.build_output_stream::<$T, _, _>(
                $cfg,
                move |data: &mut [$T], _| {
                    let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
                    let (scratch, fill) = &mut *guard;
                    if scratch.len() < data.len() {
                        scratch.resize(data.len(), 0.0);
                    }
                    let buf = &mut scratch[..data.len()];
                    fill(buf);
                    for (d, s) in data.iter_mut().zip(buf.iter()) {
                        *d = $from_f32(*s);
                    }
                },
                error_callback("speaker", on_error.clone()),
                None,
            )
        }};
    }

    let mut last_err = None;
    for buffer_size in buffer_candidates(sample_rate, pulse, false) {
        let stream_config = cpal::StreamConfig {
            channels: config.channels(),
            sample_rate,
            buffer_size,
        };
        let built = match sample_format {
            cpal::SampleFormat::F32 => {
                let state = Arc::clone(&state);
                device.build_output_stream::<f32, _, _>(
                    stream_config,
                    move |data: &mut [f32], _| {
                        let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
                        (guard.1)(data);
                    },
                    error_callback("speaker", on_error.clone()),
                    None,
                )
            }
            cpal::SampleFormat::I16 => build_output!(
                i16,
                |s: f32| (s * 32_767.0).clamp(-32_768.0, 32_767.0) as i16,
                stream_config
            ),
            cpal::SampleFormat::U16 => build_output!(
                u16,
                |s: f32| ((s + 1.0) * 32_768.0).clamp(0.0, 65_535.0) as u16,
                stream_config
            ),
            cpal::SampleFormat::I32 => build_output!(
                i32,
                |s: f32| (s * 2_147_483_647.0).clamp(-2_147_483_648.0, 2_147_483_647.0) as i32,
                stream_config
            ),
            cpal::SampleFormat::U32 => build_output!(
                u32,
                |s: f32| ((s as f64 + 1.0) * 2_147_483_648.0).clamp(0.0, 4_294_967_295.0) as u32,
                stream_config
            ),
            other => return Err(anyhow::anyhow!("unsupported speaker format: {other:?}").into()),
        };
        match built {
            Ok(stream) => {
                stream
                    .play()
                    .map_err(|e| anyhow::anyhow!("speaker stream play: {e}"))?;
                crate::diag!(
                    "[voice] speaker open @ {sample_rate} Hz × {channels} ch, buffer {buffer_size:?}"
                );
                return Ok((SendableStream(stream), sample_rate, channels));
            }
            Err(e) => {
                crate::diag!(
                    "[voice] speaker buffer {buffer_size:?} rejected ({e}); trying the next"
                );
                last_err = Some(e);
            }
        }
    }
    Err(anyhow::anyhow!(
        "build speaker stream: {}",
        last_err.map(|e| e.to_string()).unwrap_or_default()
    )
    .into())
}

/// (stream, sample rate, channels, interleaved ring).
type SpeakerStream = (SendableStream, u32, u32, Arc<Mutex<VecDeque<f32>>>);

/// Speaker for the voice-test harness, fed from an interleaved f32 ring.
pub(crate) fn start_speaker_stream(
    device: &cpal::Device,
    _preferred_rate: u32,
) -> Result<SpeakerStream> {
    let buf: Arc<Mutex<VecDeque<f32>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(48_000 * 2 * 2)));
    let buf_cb = Arc::clone(&buf);
    let (stream, sample_rate, channels) = build_output_stream(
        device,
        move |_, _| {
            move |out: &mut [f32]| {
                let mut b = buf_cb.lock().unwrap_or_else(|p| p.into_inner());
                for s in out.iter_mut() {
                    *s = b.pop_front().unwrap_or(0.0);
                }
            }
        },
        None,
    )?;
    Ok((stream, sample_rate, channels, buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(format: cpal::SampleFormat, channels: u16, min: u32, max: u32) -> cpal::SupportedStreamConfigRange {
        cpal::SupportedStreamConfigRange::new(channels, min, max, cpal::SupportedBufferSize::Unknown, format)
    }

    fn default(format: cpal::SampleFormat, channels: u16, rate: u32) -> cpal::SupportedStreamConfig {
        cpal::SupportedStreamConfig::new(channels, rate, cpal::SupportedBufferSize::Unknown, format)
    }

    #[test]
    fn a_handled_default_format_is_kept_at_48k_when_offered() {
        let configs = [range(cpal::SampleFormat::I16, 2, 8_000, 192_000)];
        let picked = pick_config(&configs, &default(cpal::SampleFormat::I16, 2, 44_100), MIX_RATE_HZ);
        assert_eq!(picked, (cpal::SampleFormat::I16, MIX_RATE_HZ));
    }

    // A PulseAudio source on S24 hardware reports I24; the server converts to f32.
    #[test]
    fn an_unhandled_default_format_opens_as_f32() {
        let configs = [
            range(cpal::SampleFormat::I24, 2, 1, 384_000),
            range(cpal::SampleFormat::F32, 2, 1, 384_000),
        ];
        let picked = pick_config(&configs, &default(cpal::SampleFormat::I24, 2, 48_000), MIX_RATE_HZ);
        assert_eq!(picked, (cpal::SampleFormat::F32, MIX_RATE_HZ));
    }

    // macOS SCO AirPods: only 16/24 kHz, so the native rate and `capture.rs` resamples.
    #[test]
    fn the_native_rate_when_48k_is_not_offered() {
        let configs = [range(cpal::SampleFormat::F32, 1, 24_000, 24_000)];
        let picked = pick_config(&configs, &default(cpal::SampleFormat::F32, 1, 24_000), MIX_RATE_HZ);
        assert_eq!(picked, (cpal::SampleFormat::F32, 24_000));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pulse_asks_for_10ms_mic_fragments_and_20ms_speaker_requests() {
        assert_eq!(buffer_candidates(48_000, true, true)[0], cpal::BufferSize::Fixed(480));
        assert_eq!(buffer_candidates(48_000, true, false)[0], cpal::BufferSize::Fixed(960));
        assert_eq!(buffer_candidates(48_000, false, true)[0], cpal::BufferSize::Fixed(1_920));
    }
}
