//! The local publish pump: one captured BGRx frame in, the encoder's frame and (when its
//! tile wants one) a self-preview frame out. Shared by the Linux/macOS helper readers, the
//! Windows WGC handler and the camera paths.

use std::sync::Arc;

use libwebrtc::{
    prelude::{VideoFrame, VideoRotation},
    video_frame::VideoBuffer,
    video_source::native::NativeVideoSource,
};

use super::{
    codec::{bound_to_limits, convert_to_i420, pack_frame_bytes, FramePacer, ShareLimits},
    wants::PreviewMirror,
    RawSink,
};

pub(crate) struct LocalVideoPump {
    /// `None` on a preview-only capture (the settings page's camera check).
    source: Option<NativeVideoSource>,
    limits: ShareLimits,
    pacer: FramePacer,
    mirror: PreviewMirror,
    preview_key: &'static str,
    preview_tx: tokio::sync::broadcast::Sender<Arc<Vec<u8>>>,
    /// The legacy Channel sink, kept until nothing registers one.
    frames: Option<Arc<dyn RawSink>>,
}

impl LocalVideoPump {
    pub fn new(
        source: Option<NativeVideoSource>,
        limits: ShareLimits,
        mirror: PreviewMirror,
        preview_key: &'static str,
        preview_tx: tokio::sync::broadcast::Sender<Arc<Vec<u8>>>,
        frames: Option<Arc<dyn RawSink>>,
    ) -> Self {
        Self {
            source,
            limits,
            pacer: FramePacer::new(limits.max_fps),
            mirror,
            preview_key,
            preview_tx,
            frames,
        }
    }

    /// One BGRx frame from the capture. Frames past the publish rate are dropped here,
    /// before the conversion, since the encoder would drop them after it.
    pub fn push(&mut self, width: u32, height: u32, stride: u32, timestamp_us: i64, bgrx: &[u8]) {
        // libwebrtc/VP8/I420 need even dims; crop down.
        let w = (width & !1) as i32;
        let h = (height & !1) as i32;
        if w <= 0 || h <= 0 {
            return;
        }
        let now = std::time::Instant::now();
        if !self.pacer.admit(now) {
            return;
        }
        // BGRx == little-endian ARGB; the limit's height is the backstop for a backend that
        // could not size its own capture.
        let buffer = bound_to_limits(convert_to_i420(w, h, stride, bgrx), &self.limits);
        let frame = VideoFrame {
            rotation: VideoRotation::VideoRotation0,
            timestamp_us,
            // No packet-trailer features are negotiated, so there is no metadata to attach.
            frame_metadata: None,
            buffer,
        };
        if let Some(source) = &self.source {
            source.capture_frame(&frame);
        }
        let mut buffer = frame.buffer;
        let Some(scale_to) = self.mirror.plan((buffer.width(), buffer.height()), now) else {
            return;
        };
        let mirrored = match scale_to {
            Some((mw, mh)) => buffer.scale(mw as i32, mh as i32),
            None => buffer,
        };
        let bytes = Arc::new(pack_frame_bytes(
            self.preview_key,
            mirrored.width(),
            mirrored.height(),
            timestamp_us,
            &mirrored,
        ));
        let _ = self.preview_tx.send(Arc::clone(&bytes));
        if let Some(sink) = &self.frames {
            let _ = sink.send((*bytes).clone());
        }
    }
}
