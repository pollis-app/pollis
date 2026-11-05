//! Linux + macOS capture lifecycle: picker, start, and BGRx→I420 frame push.
//!
//! Linux's portal dialog is the picker; macOS uses the in-app picker.

use std::sync::Arc;

use libwebrtc::{
    prelude::RtcVideoSource,
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
    codec::{pick_screenshare_codec, resolve_screenshare_encoding, ShareLimits},
    fail_capture,
    helper_subprocess::spawn_and_accept_helper,
    pump::LocalVideoPump,
    self_echo::SelfEchoCanceller,
    stop::{stop_screen_share, stop_screen_share_if},
    wants::{wants_channel, PreviewMirror},
    ScreenShareEvent, LOCAL_PREVIEW_KEY,
};

/// macOS: spawn the helper to enumerate sources and park it for the pick.
#[cfg(target_os = "macos")]
pub async fn enumerate_screen_sources(
    state: &Arc<AppState>,
) -> Result<pollis_capture_proto::SourceList> {
    use pollis_capture_proto::{read_msg, CaptureMsg};

    // Discard any previous picker session that never got chosen.
    cancel_screen_share_picker(state).await.ok();

    // Video-only, unbounded at spawn: macOS gets the audio choice and the limits in `Select`.
    let mut session = spawn_and_accept_helper(state, false, None).await?;
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
            // Verbatim: the frontend's `friendlyScreenShareError` rewrites it.
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
    // Linux: the portal picks; empty is a safer no-op than an error.
    Ok(pollis_capture_proto::SourceList {
        displays: Vec::new(),
        windows: Vec::new(),
    })
}

/// Discard a parked picker session (user backed out).
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

/// `with_audio` is the source of truth (macOS: on `Selection`; Linux: spawn flag).
/// `max_height` is the "share resolution" setting (`None`/`0` = source); with the fps it
/// bounds the capture itself, not only the publish.
pub async fn start_screen_share(
    state: &Arc<AppState>,
    selection: Option<pollis_capture_proto::Selection>,
    max_framerate: Option<u32>,
    with_audio: bool,
    max_height: Option<u32>,
) -> Result<()> {
    use pollis_capture_proto::{encode_select, CaptureLimits};
    use tokio::io::AsyncWriteExt;

    let limits = ShareLimits::resolve(max_framerate, max_height);
    let capture_limits = CaptureLimits {
        max_fps: limits.max_fps,
        max_height: limits.max_height.unwrap_or(0),
    };

    let room = {
        let voice = state.voice.lock().await;
        voice.room.clone()
    };
    let room = room.ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("not in a voice channel — join voice first"))
    })?;

    // Re-share from a clean slate.
    {
        let has_prev = {
            let ss = state.screenshare.lock().await;
            ss.local_track.is_some() || ss.local_helper.is_some() || ss.local_reader_task.is_some()
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
        None => spawn_and_accept_helper(state, with_audio, Some(capture_limits)).await?,
    };

    // Stamp the audio decision and the limits onto the selection for the parked macOS helper.
    let selection = selection.map(|sel| match sel {
        pollis_capture_proto::Selection::Display { id, .. } => {
            pollis_capture_proto::Selection::Display {
                id,
                with_audio,
                limits: capture_limits,
            }
        }
        pollis_capture_proto::Selection::Window { id, .. } => {
            pollis_capture_proto::Selection::Window {
                id,
                with_audio,
                limits: capture_limits,
            }
        }
    });

    // macOS: Select unblocks the helper parked between Sources and Format.
    if let Some(sel) = &selection {
        if let Err(e) = session.writer.write_all(&encode_select(sel)).await {
            crate::diag!("[screenshare] send Select: {e}");
            return Err(fail_capture(
                state,
                "Could not deliver the screen-share selection to the helper. Please try again."
                    .into(),
            )
            .await);
        }
        let _ = session.writer.flush().await;
    }

    // Park the session so a concurrent stop can kill it; writer stays open (EOF exits).
    // The token is re-checked after every await below: a stop that lands mid-handshake
    // must end here, not in a published track with no capture behind it.
    let token = {
        let mut ss = state.screenshare.lock().await;
        ss.local_helper = Some(session.child);
        ss.local_writer = Some(session.writer);
        ss.begin_session()
    };

    // Read until a dimension message (Format or leading Frame) or Error.
    // Tolerant loop: Wayland may send Frame first; skip other benign messages.
    let mut reader = session.reader;
    crate::diag!("[screenshare] awaiting video format from helper");
    use pollis_capture_proto::CaptureMsg;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
    // Bound junk; a healthy handshake skips at most one message.
    const MAX_SKIPPED_BEFORE_FORMAT: usize = 64;
    let mut skipped = 0usize;
    // Audio may negotiate first (Linux portal); keep it, it won't be re-sent.
    let mut early_audio_format: Option<(u32, u32)> = None;
    let (width, height) = loop {
        let read_result =
            tokio::time::timeout_at(deadline, pollis_capture_proto::read_msg(&mut reader)).await;
        let msg = match read_result {
            Ok(r) => r,
            Err(_) => {
                // 300 s with no format: the portal never produced a stream.
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
            // Leading Frame (zero-size param_changed skipped Format): use its dims.
            Ok(Some(CaptureMsg::Frame { width, height, .. })) => {
                crate::diag!(
                    "[screenshare] helper sent frame before format; deriving {}x{}",
                    width,
                    height
                );
                break (width & !1, height & !1);
            }
            // Audio first: hold its format; drop its frames until video is up.
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
                other @ (CaptureMsg::Sources(_)
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
                // Audio-only failure: downgrade rather than tear down the video share.
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
                // Prefixes: `unsupported:` (no ScreenCast backend), `cancel`/`dismiss`, else failure.
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
                // User cancelled the picker: return Ok silently (no toast).
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
                // A stop during the handshake kills the helper; that EOF is not a failure.
                if session_ended(state, token).await {
                    return Ok(());
                }
                crate::diag!("[screenshare] helper closed socket before format");
                stop_screen_share(state).await.ok();
                return Err(fail_capture(
                    state,
                    "Screen capture ended before it started. Please try again.".into(),
                )
                .await);
            }
            Err(e) => {
                if session_ended(state, token).await {
                    return Ok(());
                }
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
    if session_ended(state, token).await {
        return Ok(());
    }

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
    let track =
        LocalVideoTrack::create_video_track("screenshare", RtcVideoSource::Native(source.clone()));
    crate::diag!("[screenshare] publishing track {}x{}", width, height);
    // Pin encoding: the SDK preset caps sub-1080p shares at 15 fps.
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
            "Could not publish the screen-share to the call. Check your connection and try again."
                .into(),
        )
        .await);
    }
    crate::diag!("[screenshare] track published");
    if session_ended(state, token).await {
        // Stopped while publishing: roll the publish back; the stop already announced itself.
        let sid = track.sid();
        let _ = room.local_participant().unpublish_track(&sid).await;
        return Ok(());
    }

    // Reader task owns socket + source; on exit it runs the stop path (never a zombie share).
    // Self-preview rides the same WebSocket broadcast as remote tiles, sized and paced by
    // what its own tile reports.
    let mut pump = {
        let mut ss = state.screenshare.lock().await;
        LocalVideoPump::new(
            Some(source.clone()),
            limits,
            PreviewMirror::new(wants_channel(&mut ss, LOCAL_PREVIEW_KEY)),
            LOCAL_PREVIEW_KEY,
            state.screenshare_frame_tx.clone(),
            ss.frames.clone(),
        )
    };
    let state_for_task = Arc::clone(state);
    let room_for_task = room.clone();
    let reader_task = tokio::spawn(async move {
        let state_for_exit = Arc::clone(&state_for_task);
        // Shared-audio pump: inert until the helper announces a format.
        let mut audio_source: Option<libwebrtc::audio_source::native::NativeAudioSource> = None;
        let mut resampler: Option<SharedAudioResampler> = None;
        let mut echo: Option<SelfEchoCanceller> = None;
        // No FPS cap and no idle keep-alive: viewers keep the last frame (#300).
        // Early audio: publish before the first frame.
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
                    pump.push(width, height, stride, timestamp_us, &bgrx);
                }
                Ok(Some(CaptureMsg::Format { .. })) => {
                    // Mid-stream renegotiation: NativeVideoSource tolerates size changes.
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
                    // Follow a mid-stream rate change (output device switched).
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
                    // Picker-handshake only; ignore.
                }
                Ok(Some(CaptureMsg::Error { message })) => {
                    // Audio loss keeps the video share: drop the pump, unpublish, disarm tap (#1040).
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
                    crate::diag!("[screenshare] helper error mid-share: {message}");
                    on_reader_exit(
                        state_for_exit,
                        token,
                        "Screen capture failed. Please try sharing again.".into(),
                    );
                    break;
                }
                Ok(None) => {
                    crate::diag!("[screenshare] helper closed socket mid-share");
                    on_reader_exit(
                        state_for_exit,
                        token,
                        "Screen sharing stopped: the capture source ended.".into(),
                    );
                    break;
                }
                Err(e) => {
                    crate::diag!("[screenshare] reader error mid-share: {e}");
                    on_reader_exit(
                        state_for_exit,
                        token,
                        "Lost the screen-capture connection. Please try sharing again.".into(),
                    );
                    break;
                }
            }
        }
    });

    // Save state and announce; the helper was parked earlier.
    {
        let mut ss = state.screenshare.lock().await;
        if !ss.is_session(token) {
            drop(ss);
            reader_task.abort();
            let sid = track.sid();
            let _ = room.local_participant().unpublish_track(&sid).await;
            return Ok(());
        }
        ss.local_source = Some(source);
        ss.local_track = Some(track);
        ss.local_reader_task = Some(reader_task);
        if let Some(ev) = &ss.events {
            let _ = ev.send(ScreenShareEvent::LocalStarted { width, height });
        }
    }
    Ok(())
}

/// True once a stop has invalidated `token` (see `ScreenShareState::begin_session`).
async fn session_ended(state: &Arc<AppState>, token: u64) -> bool {
    !state.screenshare.lock().await.is_session(token)
}

/// The reader task's exit: run the stop path for this session, then tell the renderer why.
///
/// Spawned rather than awaited, because the stop aborts the reader task itself and an
/// `.await` inside the aborted task would never resume past the unpublish. `LocalStopped`
/// lands first (the share is gone), `LocalError` second (why), so the renderer never
/// keeps an `active` share with a frozen last frame at every peer.
pub(super) fn on_reader_exit(state: Arc<AppState>, token: u64, message: String) {
    tokio::spawn(async move {
        let live = state.screenshare.lock().await.is_session(token);
        if !live {
            return;
        }
        let _ = stop_screen_share_if(&state, token).await;
        let ev = {
            let ss = state.screenshare.lock().await;
            ss.events.clone()
        };
        if let Some(ev) = ev {
            let _ = ev.send(ScreenShareEvent::LocalError { message });
        }
    });
}

/// Publish the shared-audio track; a repeat `AudioFormat` only retunes.
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
    // Linux only: macOS excludes our process at the SCK layer.
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

/// Normalise one captured block and publish each whole 10 ms frame.
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
        // Cancel the call echo before LiveKit sees the frame.
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
