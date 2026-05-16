use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use cpal::traits::{DeviceTrait, StreamTrait};

use crate::error::Result;

use super::types::SendableStream;

/// Build a cpal input stream that converts mic audio to i16 mono and sends
/// 10ms chunks to `frame_tx`. Downmixes multi-channel input to mono.
pub(crate) fn start_mic_stream(
    device: &cpal::Device,
    frame_tx: tokio::sync::mpsc::UnboundedSender<(Vec<i16>, u32)>,
    is_muted: Arc<AtomicBool>,
) -> Result<(SendableStream, u32)> {
    let config = device
        .default_input_config()
        .map_err(|e| anyhow::anyhow!("input config: {e}"))?;
    let channels = config.channels() as usize;
    let sample_format = config.sample_format();
    // Prefer 48 kHz (WebRTC APM only supports 8/16/32/48 kHz), but fall
    // back to whatever the device actually supports — Bluetooth devices
    // on macOS (e.g. AirPods in SCO) often only advertise 16/24 kHz, and
    // forcing 48 kHz makes build_input_stream fail.
    let preferred_rate: u32 = 48_000;
    let supports_48k = device
        .supported_input_configs()
        .map(|cfgs| {
            cfgs.filter(|c| c.channels() == config.channels() && c.sample_format() == sample_format)
                .any(|c| {
                    c.min_sample_rate().0 <= preferred_rate
                        && c.max_sample_rate().0 >= preferred_rate
                })
        })
        .unwrap_or(false);
    let sample_rate: u32 = if supports_48k {
        preferred_rate
    } else {
        config.sample_rate().0
    };
    let stream_config = cpal::StreamConfig {
        channels: config.channels(),
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    // Build the stream for whatever native format the device reports.
    // Each arm clones the shared state so it can move into the closure.
    macro_rules! build_input {
        ($T:ty, $to_f32:expr) => {{
            let frame_tx = frame_tx.clone();
            let is_muted = Arc::clone(&is_muted);
            device.build_input_stream::<$T, _, _>(
                &stream_config,
                move |data: &[$T], _| {
                    if is_muted.load(Ordering::Relaxed) {
                        return;
                    }
                    let f32s: Vec<f32> = data.iter().copied().map($to_f32).collect();
                    let mono: Vec<i16> = if channels == 1 {
                        f32s.iter()
                            .map(|&s| (s * 32_767.0).clamp(-32_768.0, 32_767.0) as i16)
                            .collect()
                    } else {
                        f32s.chunks(channels)
                            .map(|ch| {
                                let avg = ch.iter().sum::<f32>() / channels as f32;
                                (avg * 32_767.0).clamp(-32_768.0, 32_767.0) as i16
                            })
                            .collect()
                    };
                    let _ = frame_tx.send((mono, sample_rate));
                },
                |e| eprintln!("[voice] mic error: {e}"),
                None,
            )
        }};
    }

    let stream = match sample_format {
        cpal::SampleFormat::F32 => build_input!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => build_input!(i16, |s: i16| s as f32 / 32_768.0),
        cpal::SampleFormat::U16 => build_input!(u16, |s: u16| (s as f32 - 32_768.0) / 32_768.0),
        cpal::SampleFormat::I32 => build_input!(i32, |s: i32| s as f32 / 2_147_483_648.0),
        cpal::SampleFormat::U32 => build_input!(u32, |s: u32| (s as f64 / 2_147_483_648.0 - 1.0) as f32),
        other => return Err(anyhow::anyhow!("unsupported mic format: {other:?}").into()),
    }
    .map_err(|e| anyhow::anyhow!("build mic stream: {e}"))?;

    stream.play().map_err(|e| anyhow::anyhow!("mic stream play: {e}"))?;
    Ok((SendableStream(stream), sample_rate))
}

/// Build a cpal output stream driven by a shared ring buffer.
/// Returns the stream (kept alive by the caller) and the buffer to push into.
///
/// `preferred_rate` is the rate we'd like to run at; we'll honour it when
/// the device supports it and fall back to its default rate otherwise. The
/// caller can compare the returned rate against `preferred_rate` to decide
/// whether the AEC render reference can be tapped without resampling.
pub(crate) fn start_speaker_stream(
    device: &cpal::Device,
    preferred_rate: u32,
) -> Result<(SendableStream, u32, u32, Arc<Mutex<VecDeque<f32>>>)> {
    let config = device
        .default_output_config()
        .map_err(|e| anyhow::anyhow!("output config: {e}"))?;
    let channels = config.channels() as u32;
    let sample_format = config.sample_format();

    let supports_preferred = device
        .supported_output_configs()
        .map(|cfgs| {
            cfgs.filter(|c| c.channels() == config.channels() && c.sample_format() == sample_format)
                .any(|c| {
                    c.min_sample_rate().0 <= preferred_rate
                        && c.max_sample_rate().0 >= preferred_rate
                })
        })
        .unwrap_or(false);
    let sample_rate: u32 = if supports_preferred {
        preferred_rate
    } else {
        config.sample_rate().0
    };
    let stream_config = cpal::StreamConfig {
        channels: config.channels(),
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    // 2-second ring buffer (always f32 internally; converted on output)
    let buf: Arc<Mutex<VecDeque<f32>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity((sample_rate * channels * 2) as usize)));

    macro_rules! build_output {
        ($T:ty, $from_f32:expr) => {{
            let buf_cb = Arc::clone(&buf);
            device.build_output_stream::<$T, _, _>(
                &stream_config,
                move |data: &mut [$T], _| {
                    let mut b = buf_cb.lock().unwrap();
                    for s in data.iter_mut() {
                        *s = $from_f32(b.pop_front().unwrap_or(0.0));
                    }
                },
                |e| eprintln!("[voice] speaker error: {e}"),
                None,
            )
        }};
    }

    let stream = match sample_format {
        cpal::SampleFormat::F32 => build_output!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => build_output!(i16, |s: f32| (s * 32_767.0).clamp(-32_768.0, 32_767.0) as i16),
        cpal::SampleFormat::U16 => build_output!(u16, |s: f32| ((s + 1.0) * 32_768.0).clamp(0.0, 65_535.0) as u16),
        cpal::SampleFormat::I32 => build_output!(i32, |s: f32| (s * 2_147_483_647.0).clamp(-2_147_483_648.0, 2_147_483_647.0) as i32),
        other => return Err(anyhow::anyhow!("unsupported speaker format: {other:?}").into()),
    }
    .map_err(|e| anyhow::anyhow!("build speaker stream: {e}"))?;

    stream.play().map_err(|e| anyhow::anyhow!("speaker stream play: {e}"))?;
    Ok((SendableStream(stream), sample_rate, channels, buf))
}
