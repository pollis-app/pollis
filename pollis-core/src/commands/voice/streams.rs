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
/// mapping: ALSA takes `Fixed(v)` as a `v`-frame buffer with `v/4` periods, so
/// `rate/25` is a 10 ms period under a 40 ms buffer (PipeWire's ALSA plugin
/// accepts it; measured 10.7 ms callbacks against 42.7 ms with `Default`).
/// CoreAudio sets its period to `v` directly; WASAPI shared mode takes `v`
/// as the buffer over its fixed 10 ms engine period.
fn buffer_candidates(rate: u32) -> [cpal::BufferSize; 2] {
    #[cfg(target_os = "linux")]
    let fixed = rate / 25;
    #[cfg(target_os = "macos")]
    let fixed = rate / 100;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let fixed = rate / 50;
    [cpal::BufferSize::Fixed(fixed), cpal::BufferSize::Default]
}

/// Pick `preferred` if the device advertises it for this format/channels,
/// else the device default rate.
fn pick_rate<I>(configs: Option<I>, default: &cpal::SupportedStreamConfig, preferred: u32) -> u32
where
    I: Iterator<Item = cpal::SupportedStreamConfigRange>,
{
    let supports = configs
        .map(|cfgs| {
            cfgs.filter(|c| {
                c.channels() == default.channels() && c.sample_format() == default.sample_format()
            })
            .any(|c| c.min_sample_rate().0 <= preferred && c.max_sample_rate().0 >= preferred)
        })
        .unwrap_or(false);
    if supports {
        preferred
    } else {
        default.sample_rate().0
    }
}

/// Open the mic. `sink` runs on the audio thread once per device period with
/// mono i16 at the returned rate, from a scratch buffer reused across calls.
///
/// Never gated here: mute is applied downstream so RNNoise/APM keep their
/// state and the local meter stays alive.
pub(crate) fn build_mic_stream<F>(device: &cpal::Device, sink: F) -> Result<(SendableStream, u32)>
where
    F: FnMut(&[i16], u32) + Send + 'static,
{
    let config = device
        .default_input_config()
        .map_err(|e| anyhow::anyhow!("input config: {e}"))?;
    let channels = config.channels() as usize;
    let sample_format = config.sample_format();
    // 48 kHz when the device offers it means no resampling at all; otherwise
    // the native rate (macOS SCO AirPods: 16/24 kHz) and `capture.rs` resamples.
    let sample_rate = pick_rate(device.supported_input_configs().ok(), &config, MIX_RATE_HZ);

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
                |e| crate::diag!("[voice] mic error: {e}"),
                None,
            )
        }};
    }

    let mut last_err = None;
    for buffer_size in buffer_candidates(sample_rate) {
        let stream_config = cpal::StreamConfig {
            channels: config.channels(),
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size,
        };
        let built = match sample_format {
            cpal::SampleFormat::F32 => build_input!(f32, |s: f32| s, &stream_config),
            cpal::SampleFormat::I16 => {
                build_input!(i16, |s: i16| s as f32 / 32_768.0, &stream_config)
            }
            cpal::SampleFormat::U16 => build_input!(
                u16,
                |s: u16| (s as f32 - 32_768.0) / 32_768.0,
                &stream_config
            ),
            cpal::SampleFormat::I32 => {
                build_input!(i32, |s: i32| s as f32 / 2_147_483_648.0, &stream_config)
            }
            cpal::SampleFormat::U32 => build_input!(
                u32,
                |s: u32| (s as f64 / 2_147_483_648.0 - 1.0) as f32,
                &stream_config
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
    build_mic_stream(device, move |mono, rate| {
        if is_muted.load(Ordering::Relaxed) {
            return;
        }
        let _ = frame_tx.send((mono.to_vec(), rate));
    })
}

/// Open the speaker. `make_fill(rate, channels)` builds the callback that
/// fills each interleaved f32 period; it runs on the audio thread.
///
/// Honours 48 kHz if the device offers it, else the device default rate.
pub(crate) fn build_output_stream<F, M>(
    device: &cpal::Device,
    make_fill: M,
) -> Result<(SendableStream, u32, u32)>
where
    F: FnMut(&mut [f32]) + Send + 'static,
    M: FnOnce(u32, u32) -> F,
{
    let config = device
        .default_output_config()
        .map_err(|e| anyhow::anyhow!("output config: {e}"))?;
    let channels = config.channels() as u32;
    let sample_format = config.sample_format();
    let sample_rate = pick_rate(device.supported_output_configs().ok(), &config, MIX_RATE_HZ);

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
                |e| crate::diag!("[voice] speaker error: {e}"),
                None,
            )
        }};
    }

    let mut last_err = None;
    for buffer_size in buffer_candidates(sample_rate) {
        let stream_config = cpal::StreamConfig {
            channels: config.channels(),
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size,
        };
        let built = match sample_format {
            cpal::SampleFormat::F32 => {
                let state = Arc::clone(&state);
                device.build_output_stream::<f32, _, _>(
                    &stream_config,
                    move |data: &mut [f32], _| {
                        let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
                        (guard.1)(data);
                    },
                    |e| crate::diag!("[voice] speaker error: {e}"),
                    None,
                )
            }
            cpal::SampleFormat::I16 => build_output!(
                i16,
                |s: f32| (s * 32_767.0).clamp(-32_768.0, 32_767.0) as i16,
                &stream_config
            ),
            cpal::SampleFormat::U16 => build_output!(
                u16,
                |s: f32| ((s + 1.0) * 32_768.0).clamp(0.0, 65_535.0) as u16,
                &stream_config
            ),
            cpal::SampleFormat::I32 => build_output!(
                i32,
                |s: f32| (s * 2_147_483_647.0).clamp(-2_147_483_648.0, 2_147_483_647.0) as i32,
                &stream_config
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
    let (stream, sample_rate, channels) = build_output_stream(device, move |_, _| {
        move |out: &mut [f32]| {
            let mut b = buf_cb.lock().unwrap_or_else(|p| p.into_inner());
            for s in out.iter_mut() {
                *s = b.pop_front().unwrap_or(0.0);
            }
        }
    })?;
    Ok((stream, sample_rate, channels, buf))
}
