//! Shared-audio capture on Windows: WASAPI **process loopback**, with our
//! own process tree excluded.
//!
//! The obvious implementation — `AUDCLNT_STREAMFLAGS_LOOPBACK` on the
//! default render endpoint — is wrong here, and quietly so. That endpoint
//! carries everything the machine is playing *including the call*, so the
//! shared audio would contain every remote participant's voice and send it
//! back to them a few hundred milliseconds late. On Linux that is fixed by
//! subtracting the known playback signal (`super::self_echo`); on Windows
//! it cannot be, because `webrtc-audio-processing` has no MSVC build and
//! the echo canceller there is a stub.
//!
//! So Windows takes the route Chromium takes for `getDisplayMedia({audio:
//! true})`: activate a loopback client against the
//! `VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK` pseudo-device with
//! `PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE` targeting our own
//! PID. The OS mixes every process *except* ours, so the call is absent by
//! construction rather than by subtraction.
//!
//! That requires Windows 10 20H1 (build 19041) or newer. The rest of the
//! screen-share path only needs 1803, so a machine between the two shares
//! video and reports audio unavailable — the same downgrade a Linux box
//! with no PipeWire gets. **We deliberately do not fall back to endpoint
//! loopback there**: shipping a known echo is worse than shipping silence,
//! because the person it breaks the call for is not the person who turned
//! it on.
//!
//! Scope is the whole system minus us. Per-application audio would mean
//! `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE` against the shared
//! window's owning PID — reachable, since the picker already resolves an
//! HWND — but it is a separate behaviour from what the other two platforms
//! do today, so it is not silently introduced here.

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

/// What we ask the loopback client to produce. Process loopback does not
/// support `GetMixFormat` — the caller states the format and WASAPI
/// converts — so this is a choice rather than a discovery. 48 kHz stereo
/// s16 matches what the publish path wants, leaving nothing to convert.
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
/// The activation is asynchronous with no synchronous alternative, so the
/// only way to get the `IAudioClient` is to implement this one-method COM
/// interface. It does the minimum: signal an event and let the calling
/// thread pick the result off the operation object.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationHandler {
    done: HANDLE,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        _operation: Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> WinResult<()> {
        // Deliberately does not touch the operation's result. Reading it
        // here would mean carrying the interface pointer back across a
        // thread boundary; the waiter already holds the operation and can
        // read it itself once this event fires.
        unsafe {
            let _ = SetEvent(self.done);
        }
        Ok(())
    }
}

/// Run the capture loop until `active` is cleared, pushing blocks into
/// `tx`.
///
/// `active` is the *same* per-session fence the WGC video thread uses, so
/// `stop_screen_share` ends both halves with the one store it already
/// does — there is no second flag to forget to flip.
///
/// `started` fires the moment the capture stream is actually running:
/// `Ok(())` right after `IAudioClient::Start()` succeeds, `Err(reason)` if
/// anything before that fails. The caller must not learn "it started" from
/// this function *returning* — that only happens when the share ends, so
/// waiting on the return means waiting out a timeout on every healthy
/// start while captured blocks pile up unconsumed.
pub(super) fn run_loopback_capture(
    tx: tokio::sync::mpsc::Sender<AudioBlock>,
    active: Arc<AtomicBool>,
    started: tokio::sync::oneshot::Sender<Result<(), String>>,
) {
    let mut started = Some(started);
    if let Err(e) = unsafe { run_inner(tx, active, &mut started) } {
        let msg = format!("{e}");
        match started.take() {
            // Failed before the stream came up: a startup failure the
            // caller downgrades to video-only.
            Some(s) => {
                let _ = s.send(Err(msg));
            }
            // Failed once running — the caller has long moved on.
            None => {
                eprintln!("[screenshare/audio] windows loopback ended: {msg}");
            }
        }
    }
}

unsafe fn run_inner(
    tx: tokio::sync::mpsc::Sender<AudioBlock>,
    active: Arc<AtomicBool>,
    started: &mut Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
) -> WinResult<()> {
    // Ask the OS for "everything except us". `TargetProcessId` names the
    // root of the tree to exclude, so a helper subprocess of ours is
    // excluded too — which matters, because the capture helpers are
    // separate processes.
    let mut activation = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: GetCurrentProcessId(),
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // The activation params travel as a VT_BLOB inside a PROPVARIANT.
    // windows-rs offers no typed BLOB constructor, so the union is
    // written field by field. `activation` must outlive the call below —
    // the blob is borrowed, not copied.
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

    // The handler fires on a system thread. Two seconds is far longer than
    // this activation ever takes; a timeout means the audio service is
    // wedged, which is an audio-only failure.
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
        // The expected failure on Windows 10 pre-20H1, where the
        // pseudo-device does not exist.
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

    // 200 ms buffer, in 100 ns units. Generous: the publish path rebuffers
    // to 10 ms anyway, and a deeper buffer costs nothing but tolerates a
    // scheduling hiccup without dropping a block.
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
    eprintln!(
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
        // Bounded wait rather than INFINITE so the fence is still
        // observed when the source has gone quiet and WASAPI stops
        // signalling — otherwise a stop would hang until the next sound.
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
            // A silent packet's buffer contents are undefined — WASAPI
            // sets the flag instead of zeroing — so synthesise the
            // silence rather than publishing whatever was in memory.
            let pcm: Vec<i16> = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0
                || data.is_null()
            {
                vec![0i16; samples]
            } else {
                std::slice::from_raw_parts(data.cast::<i16>(), samples).to_vec()
            };
            let _ = capture.ReleaseBuffer(frames);
            // `blocking_send` is correct here and `try_send` is not: this
            // is a plain OS thread, never a tokio worker, and dropping
            // audio under momentary back-pressure is an audible gap
            // rather than the harmless stale pixel a dropped video frame
            // would be.
            if tx
                .blocking_send(AudioBlock {
                    sample_rate: SHARED_AUDIO_RATE_HZ,
                    channels: u32::from(CAPTURE_CHANNELS),
                    pcm,
                })
                .is_err()
            {
                // Receiver gone — the share stopped.
                return Ok(());
            }
        }
    }
    Ok(())
}
