//! Webcam capture (AVFoundation) for `pollis-capture-macos`.
//!
//! An AVFoundation `AVCaptureSession`, since ScreenCaptureKit can't capture cameras:
//!
//!   1. enumerate video devices via `AVCaptureDeviceDiscoverySession`
//!      and send the list (`Cameras`); no virtual-camera filtering.
//!   2. wait for the parent's `SelectCamera` (carrying the opaque
//!      `AVCaptureDevice.uniqueID`).
//!   3. open an `AVCaptureSession` with an `AVCaptureVideoDataOutput`
//!      configured for `kCVPixelFormatType_32BGRA` and stream frames.
//!
//! Frames reuse `Format` + `Frame`: 32BGRA == the BGRx the parent expects.
//!
//! Isolated like the screen helper: an ObjC `@throw` aborts only this helper.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass};
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureConnection, AVCaptureDevice,
    AVCaptureDeviceDiscoverySession, AVCaptureDeviceInput, AVCaptureDevicePosition,
    AVCaptureDeviceType, AVCaptureDeviceTypeBuiltInWideAngleCamera,
    AVCaptureDeviceTypeContinuityCamera, AVCaptureDeviceTypeDeskViewCamera,
    AVCaptureDeviceTypeExternal, AVCaptureOutput, AVCaptureSession,
    AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaType,
    AVMediaTypeVideo,
};
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::{
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA, CVPixelBuffer,
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress,
};
use objc2_core_foundation::CFString;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSString};
use dispatch2::DispatchQueue;

use pollis_capture_proto::{
    encode_cameras, encode_error, encode_format, encode_frame_header, read_msg, CameraList,
    CameraSource, CaptureMsg,
};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;

use crate::macos::{drain_to_socket, Wire};

/// `AVMediaTypeVideo` is an `extern` static (`Option<&AVMediaType>`).
fn video_media_type() -> Result<&'static AVMediaType> {
    unsafe { AVMediaTypeVideo }.ok_or_else(|| anyhow!("AVMediaTypeVideo unavailable"))
}

pub async fn run_camera(read_half: OwnedReadHalf, mut write_half: OwnedWriteHalf) -> Result<()> {
    eprintln!("[capture-mac] enumerating cameras");
    let list = match enumerate_cameras() {
        Ok(list) => list,
        Err(e) => {
            let msg = format!("{e}");
            let _ = write_half.write_all(&encode_error(&msg)).await;
            return Err(anyhow!(msg));
        }
    };
    eprintln!("[capture-mac] enumerated {} cameras", list.cameras.len());
    write_half
        .write_all(&encode_cameras(&list))
        .await
        .context("send Cameras")?;

    let mut reader = BufReader::with_capacity(4096, read_half);
    let select = tokio::time::timeout(Duration::from_secs(300), read_msg(&mut reader))
        .await
        .map_err(|_| anyhow!("timed out waiting for camera selection"))?;
    let device_id = match select {
        Ok(Some(CaptureMsg::SelectCamera(sel))) => sel.id,
        Ok(Some(CaptureMsg::Select(_))) => {
            return Err(anyhow!("received screen Select while in camera mode"));
        }
        Ok(Some(other)) => {
            return Err(anyhow!("unexpected message before SelectCamera: {other:?}"));
        }
        Ok(None) => {
            eprintln!("[capture-mac] parent closed socket before SelectCamera — exiting");
            return Ok(());
        }
        Err(e) => return Err(anyhow!("read SelectCamera: {e}")),
    };
    eprintln!("[capture-mac] selected camera: {device_id}");

    let (tx, rx) = mpsc::channel::<Wire>(2);
    let stop = Arc::new(AtomicBool::new(false));

    // AVFoundation FFI on a blocking thread; only the String id crosses (handles aren't `Send`).
    let stop_for_cap = Arc::clone(&stop);
    let tx_for_cap = tx.clone();
    let _cap = tokio::task::spawn_blocking(move || {
        if let Err(e) = start_capture(device_id, tx_for_cap.clone(), stop_for_cap) {
            eprintln!("[capture-mac] camera capture error: {e}");
            let _ = tx_for_cap.blocking_send(Wire::Bytes(encode_error(&format!("camera: {e}"))));
        }
    });

    drain_to_socket(rx, write_half, Arc::clone(&stop)).await;
    Ok(())
}

/// Enumerate every video-capture device the OS reports.
fn enumerate_cameras() -> Result<CameraList> {
    let media = video_media_type()?;

    // Discovery returns only the types asked for, so ask for all camera-shaped ones.
    let types: [&AVCaptureDeviceType; 4] = unsafe {
        [
            AVCaptureDeviceTypeBuiltInWideAngleCamera,
            AVCaptureDeviceTypeExternal,
            AVCaptureDeviceTypeContinuityCamera,
            AVCaptureDeviceTypeDeskViewCamera,
        ]
    };
    let type_array = NSArray::from_slice(&types);

    let session = unsafe {
        AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
            &type_array,
            Some(media),
            AVCaptureDevicePosition::Unspecified,
        )
    };
    let devices = unsafe { session.devices() };

    let mut cameras = Vec::with_capacity(devices.count());
    for i in 0..devices.count() {
        let device = devices.objectAtIndex(i);
        let id = unsafe { device.uniqueID() }.to_string();
        let name = unsafe { device.localizedName() }.to_string();
        cameras.push(CameraSource { id, name });
    }
    Ok(CameraList { cameras })
}

/// Resolve the chosen device by uniqueID, open it, and run an
/// `AVCaptureSession` until `stop` is set.
fn start_capture(device_id: String, tx: mpsc::Sender<Wire>, stop: Arc<AtomicBool>) -> Result<()> {
    let media = video_media_type()?;

    // Fail fast on explicit denial; NotDetermined prompts on input creation.
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) };
    if status == AVAuthorizationStatus::Denied || status == AVAuthorizationStatus::Restricted {
        return Err(anyhow!(
            "camera permission denied — grant Camera access in System Settings → Privacy & Security"
        ));
    }

    let id_ns = NSString::from_str(&device_id);
    let device = unsafe { AVCaptureDevice::deviceWithUniqueID(&id_ns) }
        .ok_or_else(|| anyhow!("camera {device_id} no longer available"))?;

    let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
        .map_err(|e| anyhow!("open camera: {e}"))?;

    let session = unsafe { AVCaptureSession::new() };
    unsafe { session.beginConfiguration() };

    if !unsafe { session.canAddInput(&input) } {
        unsafe { session.commitConfiguration() };
        return Err(anyhow!("cannot add camera input to capture session"));
    }
    unsafe { session.addInput(&input) };

    let output = unsafe { AVCaptureVideoDataOutput::new() };
    // Force 32BGRA so the bytes match the parent's argb_to_i420 (default
    // is a YpCbCr format). Drop late frames rather than back up memory.
    unsafe { output.setVideoSettings(Some(&bgra_video_settings())) };
    unsafe { output.setAlwaysDiscardsLateVideoFrames(true) };

    let handler = FrameHandler::new(tx.clone(), Arc::clone(&stop));
    let delegate = ProtocolObject::from_ref(&*handler);
    // Serial queue for in-order frames; the output holds `handler` weakly, so it must outlive the session.
    let queue = DispatchQueue::new("xyz.pollis.camera.frames", None);
    unsafe { output.setSampleBufferDelegate_queue(Some(delegate), Some(&queue)) };

    if !unsafe { session.canAddOutput(&output) } {
        unsafe { session.commitConfiguration() };
        return Err(anyhow!("cannot add video data output to capture session"));
    }
    unsafe { session.addOutput(&output) };
    unsafe { session.commitConfiguration() };

    unsafe { session.startRunning() };
    eprintln!("[capture-mac] AVCaptureSession running");

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
    }

    eprintln!("[capture-mac] stopping AVCaptureSession");
    unsafe { session.stopRunning() };
    // Keep these alive until after stopRunning.
    drop(handler);
    drop(queue);
    Ok(())
}

/// `{ kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA }`.
fn bgra_video_settings() -> Retained<NSDictionary<NSString, objc2::runtime::AnyObject>> {
    // The key is a CFString; CFString and NSString are toll-free bridged,
    // so the pointer cast is sound.
    let key_cf: &CFString = unsafe { kCVPixelBufferPixelFormatTypeKey };
    let key: &NSString = unsafe { &*(key_cf as *const CFString as *const NSString) };
    let value = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
    let value_obj: &objc2::runtime::AnyObject = value.as_ref();
    NSDictionary::from_slices(&[key], &[value_obj])
}

/// Instance variables of the sample-buffer delegate; all `Send + Sync`.
struct FrameHandlerIvars {
    tx: mpsc::Sender<Wire>,
    stop: Arc<AtomicBool>,
    /// Gates the one-shot Format announcement (sent on the first frame,
    /// once real dimensions are known) and a diagnostic log.
    announced: AtomicBool,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "PollisCameraFrameHandler"]
    #[ivars = FrameHandlerIvars]
    struct FrameHandler;

    unsafe impl NSObjectProtocol for FrameHandler {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for FrameHandler {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        unsafe fn capture_output_did_output_sample_buffer(
            &self,
            _output: &AVCaptureOutput,
            sample_buffer: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            self.on_frame(sample_buffer);
        }
    }
);

impl FrameHandler {
    fn new(tx: mpsc::Sender<Wire>, stop: Arc<AtomicBool>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(FrameHandlerIvars {
            tx,
            stop,
            announced: AtomicBool::new(false),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn on_frame(&self, sample: &CMSampleBuffer) {
        let ivars = self.ivars();
        if ivars.stop.load(Ordering::Relaxed) {
            return;
        }
        let Some(image) = (unsafe { sample.image_buffer() }) else {
            return;
        };
        // The image buffer is a CVImageBuffer, which is the same type as
        // CVPixelBuffer for camera output.
        let pixel: &CVPixelBuffer = &image;

        if unsafe { CVPixelBufferLockBaseAddress(pixel, CVPixelBufferLockFlags::ReadOnly) } != 0 {
            return;
        }
        let width = CVPixelBufferGetWidth(pixel) as u32;
        let height = CVPixelBufferGetHeight(pixel) as u32;
        let stride = CVPixelBufferGetBytesPerRow(pixel) as u32;
        let base = CVPixelBufferGetBaseAddress(pixel);
        if base.is_null() {
            unsafe { CVPixelBufferUnlockBaseAddress(pixel, CVPixelBufferLockFlags::ReadOnly) };
            return;
        }

        // Round down to even for I420 chroma alignment.
        let even_w = width & !1;
        let even_h = height & !1;
        let byte_len = (stride as usize) * (height as usize);
        let bgra = unsafe { std::slice::from_raw_parts(base as *const u8, byte_len) };

        let timestamp_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);

        // Announce Format on the first frame, once dimensions are known.
        if !ivars.announced.swap(true, Ordering::Relaxed) {
            eprintln!(
                "[capture-mac] first camera frame: {width}x{height} stride={stride} bytes={byte_len}"
            );
            let _ = ivars
                .tx
                .try_send(Wire::Bytes(encode_format(even_w, even_h)));
        }

        let header = encode_frame_header(even_w, even_h, stride, timestamp_us, bgra.len());
        // Last-frame-wins; never block the capture queue.
        let _ = ivars.tx.try_send(Wire::Frame {
            header,
            bgrx: bgra.to_vec(),
        });

        unsafe { CVPixelBufferUnlockBaseAddress(pixel, CVPixelBufferLockFlags::ReadOnly) };
    }
}
