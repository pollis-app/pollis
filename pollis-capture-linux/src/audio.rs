//! Shared-audio capture on Linux: the default sink's monitor, via PipeWire,
//! on its own connection so it serves the X11 backend (no portal fd) too.
//!
//! Whole-system, unlike macOS's per-app audio: the portal owns the picker,
//! so the helper never learns which window was chosen. The monitor includes
//! our own call playback; the parent cancels it (`screenshare::self_echo`,
//! #884, `.codesight/wiki/capture-split.md`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use pollis_capture_proto::{now_us, CaptureMsg};
use tokio::sync::mpsc;

/// What the parent publishes, so the common case needs no conversion.
/// `param_changed` stays authoritative.
const WANT_RATE: u32 = 48_000;
const WANT_CHANNELS: u32 = 2;

/// Spawn the sink-monitor capture on its own thread.
///
/// Best-effort: every failure is reported with the non-fatal `audio:` prefix
/// and leaves video untouched.
pub fn spawn(tx: mpsc::Sender<CaptureMsg>, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("pollis-capture-pw-audio".into())
        .spawn(move || {
            eprintln!("[capture/pw-audio] thread entered");
            if let Err(e) = run(tx.clone(), Arc::clone(&stop)) {
                eprintln!("[capture/pw-audio] error: {e}");
                let _ = tx.blocking_send(CaptureMsg::Error {
                    message: format!("audio: {e}"),
                });
            }
            eprintln!("[capture/pw-audio] thread exiting");
        })
        .expect("spawn pipewire audio thread")
}

fn run(tx: mpsc::Sender<CaptureMsg>, stop: Arc<AtomicBool>) -> Result<()> {
    use pipewire as pw;
    use pw::{properties::properties, spa};

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    /// Filled by `param_changed`; `announced` suppresses no-op AudioFormats.
    struct Data {
        rate: u32,
        channels: u32,
        format: spa::param::audio::AudioFormat,
        announced: Option<(u32, u32)>,
    }
    let data = Data {
        rate: WANT_RATE,
        channels: WANT_CHANNELS,
        format: spa::param::audio::AudioFormat::S16LE,
        announced: None,
    };

    let stream = pw::stream::StreamRc::new(
        core,
        "pollis-screenshare-audio",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::STREAM_CAPTURE_SINK => "true",
            // Program material, so we aren't ducked or routed like a call.
            *pw::keys::MEDIA_ROLE => "Music",
        },
    )?;

    let mainloop_for_quit = mainloop.clone();
    let stop_for_proc = Arc::clone(&stop);
    let tx_for_proc = tx.clone();
    let tx_for_format = tx;

    let _listener = stream
        .add_local_listener_with_user_data::<Data>(data)
        .state_changed(|_, _, old, new| {
            eprintln!("[capture/pw-audio] state {:?} -> {:?}", old, new);
        })
        .param_changed(move |_, ud, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((mt, ms)) = pw::spa::param::format_utils::parse_format(param) else {
                return;
            };
            if mt != pw::spa::param::format::MediaType::Audio
                || ms != pw::spa::param::format::MediaSubtype::Raw
            {
                return;
            }
            let mut info = pw::spa::param::audio::AudioInfoRaw::new();
            if info.parse(param).is_err() {
                return;
            }
            let rate = info.rate();
            let channels = info.channels();
            if rate == 0 || channels == 0 {
                return;
            }
            ud.rate = rate;
            ud.channels = channels;
            ud.format = info.format();
            if ud.announced != Some((rate, channels)) {
                ud.announced = Some((rate, channels));
                eprintln!(
                    "[capture/pw-audio] format negotiated {:?} {rate} Hz x{channels}",
                    ud.format
                );
                let _ = tx_for_format.try_send(CaptureMsg::AudioFormat {
                    sample_rate: rate,
                    channels,
                });
            }
        })
        .process(move |stream, ud| {
            if stop_for_proc.load(Ordering::Relaxed) {
                mainloop_for_quit.quit();
                return;
            }
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let chunk_size = datas[0].chunk().size() as usize;
            let Some(slice) = datas[0].data() else {
                return;
            };
            if chunk_size == 0 || slice.len() < chunk_size {
                return;
            }
            let bytes = &slice[..chunk_size];
            let Some(pcm) = to_s16(bytes, ud.format) else {
                // Guessing an unknown layout would publish noise; drop.
                static WARNED: AtomicBool = AtomicBool::new(false);
                if !WARNED.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "[capture/pw-audio] unhandled sample format {:?} — no audio will flow",
                        ud.format
                    );
                }
                return;
            };
            if pcm.is_empty() {
                return;
            }
            // Never block PipeWire's real-time thread.
            let _ = tx_for_proc.try_send(CaptureMsg::AudioFrame {
                sample_rate: ud.rate,
                channels: ud.channels,
                timestamp_us: now_us(),
                pcm,
            });
        })
        .register()?;

    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::S16LE);
    audio_info.set_rate(WANT_RATE);
    audio_info.set_channels(WANT_CHANNELS);
    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: pw::spa::param::ParamType::EnumFormat.as_raw(),
            properties: audio_info.into(),
        }),
    )?
    .0
    .into_inner();
    let format_pod = pw::spa::pod::Pod::from_bytes(&values)
        .ok_or_else(|| anyhow!("malformed audio format pod"))?;
    let mut params = [format_pod];

    // `None` target follows the default sink if the user switches mid-share.
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    eprintln!("[capture/pw-audio] sink-monitor stream connected");

    mainloop.run();
    eprintln!("[capture/pw-audio] mainloop exited");
    Ok(())
}

/// One interleaved block to s16. A node can still negotiate `F32LE` (the
/// graph's internal format); anything else is `None`, never a guess.
// `==`, not `match`: `S16` and `S16LE` are the same value on little-endian.
// Planar `F32P` is deliberately absent: a different memory layout.
fn to_s16(bytes: &[u8], format: pipewire::spa::param::audio::AudioFormat) -> Option<Vec<i16>> {
    use pipewire::spa::param::audio::AudioFormat;

    if format == AudioFormat::S16LE || format == AudioFormat::S16 {
        return Some(
            bytes
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect(),
        );
    }
    if format == AudioFormat::F32LE {
        return Some(
            bytes
                .chunks_exact(4)
                .map(|c| {
                    let v = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                    (v * 32_767.0).clamp(-32_768.0, 32_767.0) as i16
                })
                .collect(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipewire::spa::param::audio::AudioFormat;

    #[test]
    fn s16_blocks_pass_through_byte_for_byte() {
        let src: Vec<i16> = vec![0, -1, 1, i16::MIN, i16::MAX];
        let bytes: Vec<u8> = src.iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(to_s16(&bytes, AudioFormat::S16LE), Some(src));
    }

    #[test]
    fn f32_blocks_are_scaled_into_range() {
        let src: Vec<f32> = vec![0.0, 1.0, -1.0, 0.5];
        let bytes: Vec<u8> = src.iter().flat_map(|s| s.to_le_bytes()).collect();
        let got = to_s16(&bytes, AudioFormat::F32LE).expect("f32 handled");
        assert_eq!(got[0], 0);
        assert_eq!(got[1], 32_767);
        assert_eq!(got[2], -32_767);
        assert!((got[3] - 16_383).abs() <= 1);
    }

    #[test]
    fn f32_overshoot_clamps_instead_of_wrapping() {
        let src: Vec<f32> = vec![1.5, -1.5];
        let bytes: Vec<u8> = src.iter().flat_map(|s| s.to_le_bytes()).collect();
        let got = to_s16(&bytes, AudioFormat::F32LE).expect("f32 handled");
        assert_eq!(got, vec![32_767, -32_768]);
    }

    #[test]
    fn unknown_formats_are_dropped_rather_than_misread() {
        assert!(to_s16(&[0u8; 16], AudioFormat::S24LE).is_none());
    }

    #[test]
    fn ragged_tails_are_truncated_safely() {
        assert_eq!(to_s16(&[1, 2, 3], AudioFormat::S16LE).unwrap().len(), 1);
        assert_eq!(to_s16(&[1, 2, 3], AudioFormat::F32LE).unwrap().len(), 0);
    }
}
