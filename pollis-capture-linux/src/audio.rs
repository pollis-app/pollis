//! Shared-audio capture on Linux: the default sink's monitor, via
//! PipeWire.
//!
//! This lives in its own thread and its own PipeWire connection rather
//! than riding the portal's core fd, because it has to serve **both**
//! screen-capture backends. The portal path has a core fd to share; the
//! X11/xcb path (issue #281) has none, and a session on Xorg wanting to
//! share a video's sound is not a stranger case than one on Wayland.
//! Connecting independently means one audio implementation covers both.
//!
//! ## What gets captured
//!
//! `stream.capture.sink = true` attaches the stream to the **default
//! sink's monitor** — everything the machine is playing, mixed. That is
//! deliberately coarser than macOS, where ScreenCaptureKit scopes audio to
//! the same content filter as the video and so gives per-application sound
//! for free when a window is shared. PipeWire can do per-application too,
//! but only by resolving the shared window to its output node — and on
//! Linux the portal owns the picker, so the helper is never told which
//! window the user chose. Whole-system is the honest answer available
//! here; the UI says so.
//!
//! ## The loopback problem, and why it is now solved
//!
//! The sink monitor contains **our own playback**, which during a call
//! means every remote participant's voice. Publishing that verbatim sends
//! everyone their own audio back a few hundred milliseconds late. This is
//! exactly why the earlier attempt at Linux screenshare audio was pulled
//! (see the note this module replaces in `linux.rs`), and it is not
//! fixable inside this helper: by the time the monitor is readable, our
//! contribution is already summed into the mix.
//!
//! It is fixable in the parent, which knows precisely what it played.
//! `pollis-core`'s `screenshare::self_echo` runs the frames this module
//! produces through an echo canceller whose render reference is the voice
//! mixer's own output. That is why this module may capture the whole sink
//! monitor without re-introducing the bug that shelved it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use pollis_capture_proto::{now_us, CaptureMsg};
use tokio::sync::mpsc;

/// What we ask PipeWire for. 48 kHz stereo s16 is what the parent's
/// publish path wants anyway, and asking for it here means the common case
/// needs no conversion at either end. PipeWire resamples for us if the
/// sink runs at something else, so `param_changed` is still authoritative.
const WANT_RATE: u32 = 48_000;
const WANT_CHANNELS: u32 = 2;

/// Spawn the sink-monitor capture on its own thread.
///
/// Audio is **strictly best-effort**: a machine with no PipeWire, no
/// default sink, or a locked-down session must still be able to share its
/// screen. Every failure below reports itself with the `audio:` prefix the
/// parent treats as non-fatal and then lets the thread end, leaving video
/// untouched.
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
        // A thread we cannot even spawn is a resource problem, not an
        // audio problem, and the caller has already decided not to fail
        // the share over audio. Panicking here would take the share down.
        .expect("spawn pipewire audio thread")
}

fn run(tx: mpsc::Sender<CaptureMsg>, stop: Arc<AtomicBool>) -> Result<()> {
    use pipewire as pw;
    use pw::{properties::properties, spa};

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    // Our own connection to the session's PipeWire daemon — not the
    // portal's fd. See the module docs: the X11 backend has no portal fd
    // at all, and audio must work there too.
    let core = context.connect_rc(None)?;

    /// Negotiated format, filled by `param_changed` before any buffer
    /// arrives. `announced` guards against re-sending AudioFormat on a
    /// renegotiation that didn't actually change anything.
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
            // The line that turns this from "record the microphone" into
            // "record what the speakers are playing".
            *pw::keys::STREAM_CAPTURE_SINK => "true",
            // Tells the session manager this is program material, not
            // speech, so it does not duck or route us like a call stream.
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
                // A format we didn't ask for and can't read. Dropping is
                // the right call: guessing the layout would publish noise.
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
            // try_send, matching the video path: a stalled socket drops a
            // block rather than blocking PipeWire's real-time thread.
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

    // `None` target: let the session manager attach us to whatever the
    // default sink currently is, so switching output device mid-share
    // follows the user rather than stranding us on the old device.
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

/// Convert one interleaved PipeWire block to the interleaved s16 the wire
/// protocol carries.
///
/// We ask for `S16LE` and PipeWire converts for us in almost every case,
/// but a node can still negotiate `F32LE` — it is the format the graph
/// runs in internally, so it is the one a passthrough-minded session
/// manager reaches for. Handling both is a dozen lines and avoids a silent
/// no-audio share on those setups. Anything else returns `None` rather
/// than guessing at a layout.
// Compared with `==` rather than matched: `AudioFormat::S16` and
// `AudioFormat::S16LE` are the same value on a little-endian target, so
// listing both as patterns is an unreachable-arm warning rather than the
// documentation it looks like.
//
// Note the asymmetry in libspa's constants — there is a native-endian
// `S16` but no native-endian `F32`, only `F32LE`/`F32BE` and the PLANAR
// `F32P`. `F32P` is deliberately absent below: it is a different memory
// layout (one buffer per channel), so accepting it here would read
// interleaved samples out of planar data and publish noise.
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

    /// Out-of-range float input must clamp rather than wrap: an
    /// unclamped `as i16` cast of a hot sample turns a loud passage into
    /// full-scale noise of the opposite sign.
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

    /// A trailing partial sample is discarded, not read past the end.
    #[test]
    fn ragged_tails_are_truncated_safely() {
        assert_eq!(to_s16(&[1, 2, 3], AudioFormat::S16LE).unwrap().len(), 1);
        assert_eq!(to_s16(&[1, 2, 3], AudioFormat::F32LE).unwrap().len(), 0);
    }
}
