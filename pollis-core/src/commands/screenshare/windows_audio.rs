//! Shared-audio capture on Windows: WASAPI **process loopback**, with our
//! own process tree excluded.
//!
//! Endpoint loopback would carry the call itself back to every participant,
//! and Windows has no echo canceller to subtract it (`webrtc-audio-processing`
//! has no MSVC build). So, like Chromium's `getDisplayMedia({audio: true})`,
//! this activates `VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK` excluding our own
//! process tree: the call is absent by construction.
//!
//! Needs Windows 10 20H1 (19041); older builds share video and report audio
//! unavailable. Deliberately no endpoint-loopback fallback: a known echo
//! breaks the call for people other than the one who turned it on. Scope is
//! the whole system minus us, as on the other platforms.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use windows::core::{implement, Interface, Ref, Result as WinResult, HRESULT};
use windows::Win32::Foundation::{HANDLE, S_OK, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX, WAVE_FORMAT_PCM,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::BLOB;
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcessId, SetEvent, WaitForSingleObject,
};
use windows::Win32::System::Variant::VT_BLOB;

use super::audio::SHARED_AUDIO_RATE_HZ;

/// The format we ask for. Process loopback has no `GetMixFormat`: the caller
/// states a format and WASAPI converts, so 48 kHz stereo s16 leaves nothing
/// to convert.
const CAPTURE_CHANNELS: u16 = 2;
const CAPTURE_BITS: u16 = 16;

/// One captured block, handed to the publish loop.
pub(super) struct AudioBlock {
    pub sample_rate: u32,
    pub channels: u32,
    pub pcm: Vec<i16>,
}

/// Completion handler for `ActivateAudioInterfaceAsync`.
///
/// The activation is async-only, so getting the `IAudioClient` means
/// implementing this COM interface. It only signals an event.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationHandler {
    done: HANDLE,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        _operation: Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> WinResult<()> {
        // The waiter holds the operation and reads the result itself, so no
        // interface pointer crosses threads.
        unsafe {
            let _ = SetEvent(self.done);
        }
        Ok(())
    }
}

/// Run the capture loop until `active` is cleared, pushing blocks into `tx`.
///
/// `active` is the same per-session fence the WGC video thread uses, so one
/// store stops both. `started` fires once `IAudioClient::Start()` succeeds (or
/// with the error before that): this function only returns when the share
/// ends, so waiting on its return would stall every healthy start.
pub(super) fn run_loopback_capture(
    tx: tokio::sync::mpsc::Sender<AudioBlock>,
    active: Arc<AtomicBool>,
    started: tokio::sync::oneshot::Sender<Result<(), String>>,
) {
    let mut started = Some(started);
    if let Err(e) = unsafe { run_inner(tx, active, &mut started) } {
        let msg = format!("{e}");
        match started.take() {
            // Failed before the stream came up: the caller downgrades to
            // video-only.
            Some(s) => {
                let _ = s.send(Err(msg));
            }
            None => {
                crate::diag!("[screenshare/audio] windows loopback ended: {msg}");
            }
        }
    }
}

unsafe fn run_inner(
    tx: tokio::sync::mpsc::Sender<AudioBlock>,
    active: Arc<AtomicBool>,
    started: &mut Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
) -> WinResult<()> {
    // Everything except our process tree, which includes our capture helper
    // subprocesses.
    let mut activation = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: GetCurrentProcessId(),
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // The params travel as a VT_BLOB in a PROPVARIANT, written field by field
    // (no typed constructor). The blob is borrowed: `activation` must outlive
    // the call.
    let mut prop = PROPVARIANT::default();
    {
        let inner = &mut *prop.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob = BLOB {
            cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            pBlobData: (&mut activation as *mut AUDIOCLIENT_ACTIVATION_PARAMS).cast(),
        };
    }

    let done = CreateEventW(None, false, false, None)?;
    let handler: IActivateAudioInterfaceCompletionHandler =
        ActivationHandler { done }.into();
    let operation: IActivateAudioInterfaceAsyncOperation = ActivateAudioInterfaceAsync(
        VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        &IAudioClient::IID,
        Some(&prop),
        &handler,
    )?;

    // Two seconds is far beyond a normal activation; a timeout means the audio
    // service is wedged (an audio-only failure).
    if WaitForSingleObject(done, 2_000) != WAIT_OBJECT_0 {
        return Err(windows::core::Error::new(
            HRESULT(-1),
            "timed out activating the process-loopback audio client",
        ));
    }

    let mut activate_result = HRESULT(0);
    let mut interface: Option<windows::core::IUnknown> = None;
    operation.GetActivateResult(&mut activate_result, &mut interface)?;
    if activate_result != S_OK {
        // Expected before Windows 10 20H1: the pseudo-device does not exist.
        return Err(windows::core::Error::new(
            activate_result,
            "process-loopback audio capture is unavailable on this version of Windows",
        ));
    }
    let audio_client: IAudioClient = interface
        .ok_or_else(|| {
            windows::core::Error::new(HRESULT(-1), "audio activation returned no interface")
        })?
        .cast()?;

    let block_align = CAPTURE_CHANNELS * CAPTURE_BITS / 8;
    let format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: CAPTURE_CHANNELS,
        nSamplesPerSec: SHARED_AUDIO_RATE_HZ,
        nAvgBytesPerSec: SHARED_AUDIO_RATE_HZ * u32::from(block_align),
        nBlockAlign: block_align,
        wBitsPerSample: CAPTURE_BITS,
        cbSize: 0,
    };

    // 200 ms buffer, in 100 ns units: the publish path rebuffers to 10 ms, and
    // depth tolerates a scheduling hiccup.
    const BUFFER_DURATION_100NS: i64 = 200 * 10_000;
    audio_client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        BUFFER_DURATION_100NS,
        0,
        &format,
        None,
    )?;

    let ready = CreateEventW(None, false, false, None)?;
    audio_client.SetEventHandle(ready)?;
    let capture: IAudioCaptureClient = audio_client.GetService()?;
    audio_client.Start()?;
    crate::diag!(
        "[screenshare/audio] WASAPI process loopback started \
         ({SHARED_AUDIO_RATE_HZ} Hz x{CAPTURE_CHANNELS}, own process tree excluded)"
    );
    if let Some(s) = started.take() {
        let _ = s.send(Ok(()));
    }

    let result = capture_loop(&capture, ready, &tx, &active);
    let _ = audio_client.Stop();
    result
}

unsafe fn capture_loop(
    capture: &IAudioCaptureClient,
    ready: HANDLE,
    tx: &tokio::sync::mpsc::Sender<AudioBlock>,
    active: &Arc<AtomicBool>,
) -> WinResult<()> {
    while active.load(Ordering::Acquire) {
        // Bounded wait, so the fence is observed even when WASAPI stops
        // signalling on silence.
        if WaitForSingleObject(ready, 200) != WAIT_OBJECT_0 {
            continue;
        }
        loop {
            if !active.load(Ordering::Acquire) {
                return Ok(());
            }
            let Ok(packet_frames) = capture.GetNextPacketSize() else {
                return Ok(());
            };
            if packet_frames == 0 {
                break;
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            if capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                .is_err()
            {
                return Ok(());
            }
            let samples = frames as usize * CAPTURE_CHANNELS as usize;
            // A silent packet's buffer is undefined (WASAPI flags it rather
            // than zeroing), so synthesise silence.
            let pcm: Vec<i16> = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0
                || data.is_null()
            {
                vec![0i16; samples]
            } else {
                std::slice::from_raw_parts(data.cast::<i16>(), samples).to_vec()
            };
            let _ = capture.ReleaseBuffer(frames);
            // `blocking_send`, not `try_send`: this is a plain OS thread, and a
            // dropped audio block is an audible gap.
            if tx
                .blocking_send(AudioBlock {
                    sample_rate: SHARED_AUDIO_RATE_HZ,
                    channels: u32::from(CAPTURE_CHANNELS),
                    pcm,
                })
                .is_err()
            {
                return Ok(());
            }
        }
    }
    Ok(())
}
