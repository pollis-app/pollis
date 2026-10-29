//! Linux + macOS capture lifecycle: enumerate (macOS in-app picker), cancel a
//! picker session, start a capture, and convert helper BGRx frames to I420.
//! On Linux the xdg-desktop-portal dialog is the picker, so enumeration is
//! empty and the frontend skips its own.

use std::sync::Arc;

use libwebrtc::{
    prelude::{RtcVideoSource, VideoFrame, VideoRotation},
    video_frame::VideoBuffer,
    video_source::{native::NativeVideoSource, VideoResolution},
};
use livekit::{
    options::{TrackPublishOptions, VideoEncoding},
    prelude::*,
    track::{LocalTrack, LocalVideoTrack},
};

use crate::{error::Result, state::AppState};

use super::{
    audio::{
        audio_error_message, emit, publish_shared_audio_track, unpublish_shared_audio,
        SharedAudioResampler, SHARED_AUDIO_FRAME_SAMPLES, SHARED_AUDIO_RATE_HZ,
    },
    codec::{convert_to_i420, pack_frame_bytes, pick_screenshare_codec, resolve_screenshare_encoding},
    fail_capture,
    helper_subprocess::spawn_and_accept_helper,
    self_echo::SelfEchoCanceller,
    stop::stop_screen_share,
    RawSink, ScreenShareEvent, LOCAL_PREVIEW_KEY, PREVIEW_MIN_INTERVAL,
};

/// macOS: spawn the helper, enumerate via `SCShareableContent`, and return the
/// capturable displays and windows. The helper stays parked in
/// `picker_session` for the upcoming `start_screen_share(Some(selection))`.
#[cfg(target_os = "macos")]
pub async fn enumerate_screen_sources(
    state: &Arc<AppState>,
) -> Result<pollis_capture_proto::SourceList> {
    use pollis_capture_proto::{read_msg, CaptureMsg};

    cancel_screen_share_picker(state).await.ok();

    // Video-only at spawn: the audio decision arrives in `Select`, since the
    // user has not seen the audio toggle yet.
    let mut session = spawn_and_accept_helper(state, false).await?;
    let msg = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        read_msg(&mut session.reader),
    )
    .await
    .map_err(|_| {
        crate::error::Error::Other(anyhow::anyhow!(
            "screen-capture helper did not return a source list (timed out)"
        ))
    })?;
    let list = match msg {
        Ok(Some(CaptureMsg::Sources(list))) => list,
        Ok(Some(CaptureMsg::Error { message })) => {
            // Verbatim: the frontend's `friendlyScreenShareError` shortens it.
            return Err(crate::error::Error::Other(anyhow::anyhow!(message)));
        }
        Ok(Some(other)) => {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "unexpected helper message during enumeration: {other:?}"
            )));
        }
        Ok(None) => {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "screen-capture helper exited before sending source list"
            )));
        }
        Err(e) => return Err(crate::error::Error::Other(anyhow::anyhow!(e))),
    };

    let mut ss = state.screenshare.lock().await;
    ss.picker_session = Some(session);
    Ok(list)
}

#[cfg(target_os = "linux")]
pub async fn enumerate_screen_sources(
    _state: &Arc<AppState>,
) -> Result<pollis_capture_proto::SourceList> {
    // The portal picks the source; an empty list is a safe no-op.
    Ok(pollis_capture_proto::SourceList {
        displays: Vec::new(),
        windows: Vec::new(),
    })
}

/// Discard a parked picker session (the user backed out without picking).
pub async fn cancel_screen_share_picker(state: &Arc<AppState>) -> Result<()> {
    let session = {
        let mut ss = state.screenshare.lock().await;
        ss.picker_session.take()
    };
    if let Some(mut session) = session {
        let _ = session.child.kill().await;
    }
    Ok(())
}

/// `with_audio` is the single source of truth for sharing sound. It reaches
/// the helpers by different routes (stamped on the macOS `Selection`, a Linux
/// spawn flag), so it is its own argument.
pub async fn start_screen_share(
    state: &Arc<AppState>,
    selection: Option<pollis_capture_proto::Selection>,
    max_framerate: Option<u32>,
    with_audio: bool,
) -> Result<()> {
    use pollis_capture_proto::encode_select;
    use tokio::io::AsyncWriteExt;

    let room = {
        let voice = state.voice.lock().await;
        voice.room.clone()
    };
    let room = room.ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("not in a voice channel — join voice first"))
    })?;

    // Start from a clean slate.
    {
        let has_prev = {
            let ss = state.screenshare.lock().await;
            ss.local_track.is_some()
                || ss.local_helper.is_some()
                || ss.local_reader_task.is_some()
        };
        if has_prev {
            let _ = stop_screen_share(state).await;
        }
    }

    // Reuse a parked picker session (macOS) or spawn fresh (Linux).
    let parked = {
        let mut ss = state.screenshare.lock().await;
        ss.picker_session.take()
    };
    let mut session = match parked {
        Some(s) => s,
        None => spawn_and_accept_helper(state, with_audio).await?,
    };

    // The macOS helper was spawned before the toggle existed; it learns the
    // audio decision from the Select it is waiting on.
    let selection = selection.map(|sel| match sel {
        pollis_capture_proto::Selection::Display { id, .. } => {
            pollis_capture_proto::Selection::Display { id, with_audio }
        }
        pollis_capture_proto::Selection::Window { id, .. } => {
            pollis_capture_proto::Selection::Window { id, with_audio }
        }
    });

    // macOS picker reply: the helper is parked between Sources and Format.
    if let Some(sel) = &selection {
        if let Err(e) = session.writer.write_all(&encode_select(sel)).await {
            crate::diag!("[screenshare] send Select: {e}");
            return Err(fail_capture(
                state,
                "Could not deliver the screen-share selection to the helper. Please try again.".into(),
            )
            .await);
        }
        let _ = session.writer.flush().await;
    }

    // Park the helper and writer so a concurrent `stop_screen_share` can kill
    // them. The writer stays open so the helper's read side never sees EOF.
    {
        let mut ss = state.screenshare.lock().await;
        ss.local_helper = Some(session.child);
        ss.local_writer = Some(session.writer);
    }

    // Read until a message announces the size (`Format`, or a leading
    // self-describing `Frame`), an `Error`, or a bound is hit. A tolerant loop:
    // on the portal path pipewire sends no `Format` for a not-yet-negotiated
    // zero size (`pollis-capture-linux/src/linux.rs`), so a `Frame` can come
    // first, and stray handshake messages are skipped rather than fatal. A
    // 5-minute deadline (the user at the portal picker) and a skip cap bound it.
    let mut reader = session.reader;
    crate::diag!("[screenshare] awaiting video format from helper");
    use pollis_capture_proto::CaptureMsg;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
    // A healthy handshake skips at most one message.
    const MAX_SKIPPED_BEFORE_FORMAT: usize = 64;
    let mut skipped = 0usize;
    // Audio can negotiate before video (the portal's video waits on the
    // user's pick), so remember an early announcement; none would come again.
    let mut early_audio_format: Option<(u32, u32)> = None;
    let (width, height) = loop {
        let read_result =
            tokio::time::timeout_at(deadline, pollis_capture_proto::read_msg(&mut reader)).await;
        let msg = match read_result {
            Ok(r) => r,
            Err(_) => {
                // The portal never produced a stream.
                stop_screen_share(state).await.ok();
                return Err(fail_capture(
                    state,
                    "Screen capture timed out waiting for a source. Please try again.".into(),
                )
                .await);
            }
        };
        match msg {
            Ok(Some(CaptureMsg::Format { width, height })) => {
                crate::diag!("[screenshare] helper announced {}x{}", width, height);
                break (width & !1, height & !1);
            }
            // A leading Frame stands in for Format (see above); its payload is
            // dropped and the next frame feeds the publish loop.
            Ok(Some(CaptureMsg::Frame { width, height, .. })) => {
                crate::diag!(
                    "[screenshare] helper sent frame before format; deriving {}x{}",
                    width, height
                );
                break (width & !1, height & !1);
            }
            // Audio negotiated first: hold the format; drop frames until video
            // is up.
            Ok(Some(CaptureMsg::AudioFormat {
                sample_rate,
                channels,
            })) => {
                crate::diag!(
                    "[screenshare/audio] helper announced {sample_rate} Hz x{channels} \
                     before video format"
                );
                early_audio_format = Some((sample_rate, channels));
                continue;
            }
            Ok(Some(CaptureMsg::AudioFrame { .. })) => {
                continue;
            }
            Ok(Some(
                other
                @ (CaptureMsg::Sources(_)
                    | CaptureMsg::Select(_)
                    | CaptureMsg::Cameras(_)
                    | CaptureMsg::SelectCamera(_)),
            )) => {
                skipped += 1;
                let kind = match other {
                    CaptureMsg::Sources(_) => "sources",
                    CaptureMsg::Select(_) => "select",
                    CaptureMsg::Cameras(_) => "cameras",
                    _ => "select-camera",
                };
                crate::diag!(
                    "[screenshare] skipping unexpected '{kind}' message before format \
                     (skipped {skipped}); still waiting for format"
                );
                if skipped > MAX_SKIPPED_BEFORE_FORMAT {
                    crate::diag!(
                        "[screenshare] helper sent {skipped} non-format messages before any \
                         format — giving up"
                    );
                    stop_screen_share(state).await.ok();
                    return Err(fail_capture(
                        state,
                        "Screen capture failed (protocol error). Please try again.".into(),
                    )
                    .await);
                }
                continue;
            }
            Ok(Some(CaptureMsg::Error { message })) => {
                // Audio-only failure: downgrade, the video share still works.
                if let Some(reason) = audio_error_message(&message) {
                    crate::diag!("[screenshare/audio] unavailable: {reason}");
                    early_audio_format = None;
                    emit(
                        state,
                        ScreenShareEvent::LocalAudioUnavailable {
                            message: reason.to_string(),
                        },
                    )
                    .await;
                    continue;
                }
                // `unsupported:` means no ScreenCast backend (e.g. Cinnamon on
                // Wayland), which the user cannot grant; `cancel` / `dismiss`
                // is a normal cancellation; anything else is a real failure.
                crate::diag!("[screenshare] helper error: {message}");
                stop_screen_share(state).await.ok();
                let lower = message.to_lowercase();
                if lower.starts_with("unsupported:") || lower.contains("no screencast") {
                    let human = "Screen sharing isn't available on this desktop. \
                        Your desktop environment doesn't provide a screen-sharing \
                        backend (xdg-desktop-portal ScreenCast). GNOME, KDE or an \
                        X11 session support it."
                        .to_string();
                    let ev = {
                        let ss = state.screenshare.lock().await;
                        ss.events.clone()
                    };
                    if let Some(ev) = ev {
                        let _ = ev.send(ScreenShareEvent::LocalUnsupported {
                            message: human.clone(),
                        });
                    }
                    return Err(crate::error::Error::Other(anyhow::anyhow!(human)));
                }
                // User cancelled the picker (`cancel:`): already cleaned up;
                // return Ok so no toast appears and no LocalStarted was sent.
                if lower.starts_with("cancel:")
                    || lower.starts_with("cancelled")
                    || lower.contains("dismiss")
                {
                    return Ok(());
                }
                return Err(fail_capture(
                    state,
                    "Screen capture could not start. Check screen-capture permission and try again.".into(),
                )
                .await);
            }
            Ok(None) => {
                crate::diag!("[screenshare] helper closed socket before format");
                stop_screen_share(state).await.ok();
                return Err(fail_capture(
                    state,
                    "Screen capture ended before it started. Please try again.".into(),
                )
                .await);
            }
            Err(e) => {
                crate::diag!("[screenshare] helper read error: {e}");
                stop_screen_share(state).await.ok();
                return Err(fail_capture(
                    state,
                    "Lost the screen-capture connection. Please try again.".into(),
                )
                .await);
            }
        }
    };

    if width == 0 || height == 0 {
        crate::diag!("[screenshare] helper announced zero-size format");
        stop_screen_share(state).await.ok();
        return Err(fail_capture(
            state,
            "Screen capture returned an invalid source size. Please try again.".into(),
        )
        .await);
    }

    let source = NativeVideoSource::new(
        VideoResolution { width, height },
        true, /* is_screencast */
    );
    let track = LocalVideoTrack::create_video_track(
        "screenshare",
        RtcVideoSource::Native(source.clone()),
    );
    crate::diag!("[screenshare] publishing track {}x{}", width, height);
    // Pin the encoding: with `None` the SDK's screenshare presets cap any
    // surface ≤1280px wide at 15fps. Honour the user's framerate preference
    // instead; it is a ceiling, so static content costs nothing.
    let (max_framerate, max_bitrate) = resolve_screenshare_encoding(max_framerate);
    if let Err(e) = room
        .local_participant()
        .publish_track(
            LocalTrack::Video(track.clone()),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                video_codec: pick_screenshare_codec(),
                video_encoding: Some(VideoEncoding {
                    max_framerate,
                    max_bitrate,
                }),
                ..Default::default()
            },
        )
        .await
    {
        crate::diag!("[screenshare] publish error: {e}");
        stop_screen_share(state).await.ok();
        return Err(fail_capture(
            state,
            "Could not publish the screen-share to the call. Check your connection and try again.".into(),
        )
        .await);
    }
    crate::diag!("[screenshare] track published");

    // The reader task owns the socket and source; on EOF or error it exits and
    // leaves the rest to stop_screen_share.
    let source_for_task = source.clone();
    // The self-preview rides the same WebSocket broadcast as remote tiles.
    let preview_tx = state.screenshare_frame_tx.clone();
    let (events_for_task, frames_for_task) = {
        let ss = state.screenshare.lock().await;
        (ss.events.clone(), ss.frames.clone())
    };
    let state_for_task = Arc::clone(state);
    let room_for_task = room.clone();
    let reader_task = tokio::spawn(async move {
        let mut last_preview: Option<std::time::Instant> = None;
        // Shared-audio pump, inert until the helper announces an audio format.
        let mut audio_source: Option<libwebrtc::audio_source::native::NativeAudioSource> = None;
        let mut resampler: Option<SharedAudioResampler> = None;
        let mut echo: Option<SelfEchoCanceller> = None;
        // No FPS cap and no keep-alive: frames go out at the source's native
        // rate, and an idle surface simply stops them (#300). Audio that
        // negotiated before video is published before the first frame.
        if let Some((sample_rate, channels)) = early_audio_format {
            start_shared_audio(
                &state_for_task,
                &room_for_task,
                sample_rate,
                channels,
                &mut audio_source,
                &mut resampler,
                &mut echo,
            )
            .await;
        }

        loop {
            match pollis_capture_proto::read_msg(&mut reader).await {
                Ok(Some(CaptureMsg::Frame {
                    width,
                    height,
                    stride,
                    timestamp_us,
                    bgrx,
                })) => {
                    // The WS preview runs at full rate; only the legacy RawSink
                    // keeps the PREVIEW_MIN_INTERVAL gate.
                    let show_preview = last_preview
                        .is_none_or(|t| t.elapsed() >= PREVIEW_MIN_INTERVAL);
                    if show_preview {
                        last_preview = Some(std::time::Instant::now());
                    }
                    let preview = if show_preview {
                        frames_for_task.as_ref().map(|s| s.as_ref())
                    } else {
                        None
                    };
                    let preview_ws = Some(&preview_tx);
                    push_frame(
                        &source_for_task,
                        width,
                        height,
                        stride,
                        timestamp_us,
                        &bgrx,
                        preview,
                        preview_ws,
                    );
                }
                Ok(Some(CaptureMsg::Format { .. })) => {
                    // Mid-stream renegotiation: the next frame carries the new
                    // size, which NativeVideoSource tolerates.
                }
                Ok(Some(CaptureMsg::AudioFormat {
                    sample_rate,
                    channels,
                })) => {
                    start_shared_audio(
                        &state_for_task,
                        &room_for_task,
                        sample_rate,
                        channels,
                        &mut audio_source,
                        &mut resampler,
                        &mut echo,
                    )
                    .await;
                }
                Ok(Some(CaptureMsg::AudioFrame {
                    sample_rate,
                    channels,
                    pcm,
                    ..
                })) => {
                    // The rate can change mid-stream (output device switch).
                    if let Some(r) = resampler.as_mut() {
                        if r.src_rate() != sample_rate {
                            crate::diag!(
                                "[screenshare/audio] source rate changed {} -> {sample_rate} Hz",
                                r.src_rate()
                            );
                            r.set_src_rate(sample_rate);
                        }
                    }
                    push_shared_audio(
                        audio_source.as_ref(),
                        resampler.as_mut(),
                        echo.as_ref(),
                        &pcm,
                        channels,
                    )
                    .await;
                }
                Ok(Some(CaptureMsg::Sources(_)))
                | Ok(Some(CaptureMsg::Select(_)))
                | Ok(Some(CaptureMsg::Cameras(_)))
                | Ok(Some(CaptureMsg::SelectCamera(_))) => {
                    // Handshake-only messages.
                }
                Ok(Some(CaptureMsg::Error { message })) => {
                    // Losing audio must not end the share. Unpublish the track
                    // and disarm the self-echo tap (#1040), then drop the
                    // source, as `stop` orders it. A later `AudioFormat`
                    // republishes.
                    if let Some(reason) = audio_error_message(&message) {
                        crate::diag!("[screenshare/audio] stopped mid-share: {reason}");
                        resampler = None;
                        echo = None;
                        unpublish_shared_audio(&state_for_task, &room_for_task).await;
                        audio_source = None;
                        emit(
                            &state_for_task,
                            ScreenShareEvent::LocalAudioUnavailable {
                                message: reason.to_string(),
                            },
                        )
                        .await;
                        continue;
                    }
                    if let Some(ev) = &events_for_task {
                        let _ = ev.send(ScreenShareEvent::LocalError { message });
                    }
                    break;
                }
                Ok(None) => break,
                Err(e) => {
                    if let Some(ev) = &events_for_task {
                        let _ = ev.send(ScreenShareEvent::LocalError {
                            message: format!("read: {e}"),
                        });
                    }
                    break;
                }
            }
        }
    });

    {
        let mut ss = state.screenshare.lock().await;
        ss.local_source = Some(source);
        ss.local_track = Some(track);
        ss.local_reader_task = Some(reader_task);
        if let Some(ev) = &ss.events {
            let _ = ev.send(ScreenShareEvent::LocalStarted { width, height });
        }
    }
    Ok(())
}

/// Publish the shared-audio track and start its pump. A second `AudioFormat`
/// only retunes the resampler: republishing would give viewers a new track id
/// for the same sound.
async fn start_shared_audio(
    state: &Arc<AppState>,
    room: &Arc<Room>,
    sample_rate: u32,
    channels: u32,
    audio_source: &mut Option<libwebrtc::audio_source::native::NativeAudioSource>,
    resampler: &mut Option<SharedAudioResampler>,
    echo: &mut Option<SelfEchoCanceller>,
) {
    crate::diag!("[screenshare/audio] helper announced {sample_rate} Hz x{channels}");
    if audio_source.is_some() {
        if let Some(r) = resampler.as_mut() {
            r.set_src_rate(sample_rate);
        }
        return;
    }
    let Some(source) = publish_shared_audio_track(state, room).await else {
        return;
    };
    *resampler = Some(SharedAudioResampler::new(sample_rate));
    // Linux only: macOS excludes our process at the ScreenCaptureKit layer,
    // and an echo canceller would only chew into the shared material.
    #[cfg(target_os = "linux")]
    {
        let slot = {
            let voice = state.voice.lock().await;
            Arc::clone(&voice.shared_audio_render)
        };
        *echo = SelfEchoCanceller::new(&slot);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = &echo;
    }
    *audio_source = Some(source);
}

/// Normalise one captured block and hand each whole 10 ms frame to LiveKit.
/// The helper's `timestamp_us` is dropped: an audio frame's timing is its
/// sample count.
async fn push_shared_audio(
    audio_source: Option<&libwebrtc::audio_source::native::NativeAudioSource>,
    resampler: Option<&mut SharedAudioResampler>,
    echo: Option<&SelfEchoCanceller>,
    pcm: &[i16],
    channels: u32,
) {
    let (Some(source), Some(resampler)) = (audio_source, resampler) else {
        return;
    };
    for mut frame in resampler.push(pcm, channels) {
        // Subtract the call before publishing.
        if let Some(echo) = echo {
            echo.process(&mut frame);
        }
        let frame = libwebrtc::prelude::AudioFrame {
            data: frame.into(),
            sample_rate: SHARED_AUDIO_RATE_HZ,
            num_channels: 1,
            samples_per_channel: SHARED_AUDIO_FRAME_SAMPLES as u32,
        };
        if let Err(e) = source.capture_frame(&frame).await {
            crate::diag!("[screenshare/audio] capture_frame error: {e:?}");
        }
    }
}

// A frame's geometry, timing and two optional sinks — a per-frame hot path where
// packing these into a struct would allocate or copy on every frame.
#[allow(clippy::too_many_arguments)]
fn push_frame(
    source: &NativeVideoSource,
    width: u32,
    height: u32,
    stride: u32,
    timestamp_us: i64,
    bgrx: &[u8],
    preview: Option<&dyn RawSink>,
    preview_ws: Option<&tokio::sync::broadcast::Sender<std::sync::Arc<Vec<u8>>>>,
) {
    // libwebrtc, VP8 and libyuv I420 require even dimensions: crop down.
    let w = (width & !1) as i32;
    let h = (height & !1) as i32;
    if w <= 0 || h <= 0 {
        return;
    }
    let buffer = convert_to_i420(w, h, stride, bgrx);
    let (out_w, out_h) = (buffer.width(), buffer.height());
    let frame = VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us,
        frame_metadata: None,
        buffer,
    };
    source.capture_frame(&frame);
    if preview.is_some() || preview_ws.is_some() {
        let bytes = pack_frame_bytes(
            LOCAL_PREVIEW_KEY,
            out_w,
            out_h,
            timestamp_us,
            &frame.buffer,
        );
        let bytes = std::sync::Arc::new(bytes);
        if let Some(tx) = preview_ws {
            let _ = tx.send(bytes.clone());
        }
        if let Some(sink) = preview {
            let _ = sink.send((*bytes).clone());
        }
    }
}
