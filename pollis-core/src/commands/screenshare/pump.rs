//! The local publish pump: one captured frame in, the encoder's frame and (when its tile
//! wants one) a self-preview frame out. Shared by the Linux/macOS helper readers, the
//! Windows WGC handler and the camera paths.
//!
//! The helper readers drive it through a [`PumpHandle`]: the socket task only reads and
//! offers, a converter thread does the libyuv/scale/pack work, and a frame that arrives
//! while the converter is busy replaces the one waiting (newest wins, bounded by one).
//! The read buffers come back through the handle, so a steady stream allocates nothing
//! per frame on the parent side.

use std::sync::{Arc, Condvar, Mutex};

use libwebrtc::{
    prelude::{VideoFrame, VideoRotation},
    video_frame::{I420Buffer, VideoBuffer},
    video_source::native::NativeVideoSource,
};

use super::{
    codec::{bound_to_limits, convert_to_i420, pack_frame_bytes, FramePacer, ShareLimits},
    wants::PreviewMirror,
};

pub(crate) struct LocalVideoPump {
    /// `None` on a preview-only capture (the settings page's camera check).
    source: Option<NativeVideoSource>,
    limits: ShareLimits,
    pacer: FramePacer,
    mirror: PreviewMirror,
    preview_key: &'static str,
    preview_tx: Arc<crate::media_server::FrameFanout>,
}

impl LocalVideoPump {
    pub fn new(
        source: Option<NativeVideoSource>,
        limits: ShareLimits,
        mirror: PreviewMirror,
        preview_key: &'static str,
        preview_tx: Arc<crate::media_server::FrameFanout>,
    ) -> Self {
        Self {
            source,
            limits,
            pacer: FramePacer::new(limits.max_fps),
            mirror,
            preview_key,
            preview_tx,
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
        // BGRx == little-endian ARGB.
        self.publish(convert_to_i420(w, h, stride, bgrx), timestamp_us, now);
    }

    /// One tight-plane I420 frame (`pollis_capture_proto::CaptureMsg::FrameI420`): copied
    /// into the encoder's buffer, no conversion.
    pub fn push_i420(&mut self, width: u32, height: u32, timestamp_us: i64, planes: &[u8]) {
        let w = width & !1;
        let h = height & !1;
        if w == 0 || h == 0 {
            return;
        }
        let (y_len, c_len) = pollis_capture_proto::i420_plane_lengths(w, h);
        if planes.len() < y_len + 2 * c_len {
            return;
        }
        let now = std::time::Instant::now();
        if !self.pacer.admit(now) {
            return;
        }
        let mut buffer = I420Buffer::new(w, h);
        {
            let (sy, su, sv) = buffer.strides();
            let (dy, du, dv) = buffer.data_mut();
            let cw = (w / 2) as usize;
            copy_plane(&planes[..y_len], w as usize, dy, sy as usize);
            copy_plane(&planes[y_len..y_len + c_len], cw, du, su as usize);
            copy_plane(
                &planes[y_len + c_len..y_len + 2 * c_len],
                cw,
                dv,
                sv as usize,
            );
        }
        self.publish(buffer, timestamp_us, now);
    }

    /// The limit's height is the backstop for a backend that could not size its own capture.
    fn publish(&mut self, buffer: I420Buffer, timestamp_us: i64, now: std::time::Instant) {
        let buffer = bound_to_limits(buffer, &self.limits);
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
        self.preview_tx.send(bytes);
    }
}

/// Tight rows into a (possibly padded) destination plane.
fn copy_plane(src: &[u8], row_len: usize, dst: &mut [u8], dst_stride: usize) {
    if dst_stride == row_len {
        dst[..src.len()].copy_from_slice(src);
        return;
    }
    for (row, chunk) in src.chunks_exact(row_len).enumerate() {
        let at = row * dst_stride;
        dst[at..at + row_len].copy_from_slice(chunk);
    }
}

/// A frame as the socket reader received it; the `Vec` is returned for reuse.
pub(crate) enum FrameIn {
    Bgrx {
        width: u32,
        height: u32,
        stride: u32,
        timestamp_us: i64,
        bytes: Vec<u8>,
    },
    I420 {
        width: u32,
        height: u32,
        timestamp_us: i64,
        planes: Vec<u8>,
    },
}

impl FrameIn {
    fn into_buffer(self) -> Vec<u8> {
        match self {
            FrameIn::Bgrx { bytes, .. } => bytes,
            FrameIn::I420 { planes, .. } => planes,
        }
    }
}

struct Slot {
    next: Option<FrameIn>,
    /// Buffers the converter has finished with; the reader takes them for the next read.
    spare: Vec<Vec<u8>>,
    closed: bool,
}

struct Shared {
    slot: Mutex<Slot>,
    cv: Condvar,
}

/// The reader's end of a converter thread (see the module docs). Dropping it ends the thread.
pub(crate) struct PumpHandle {
    shared: Arc<Shared>,
}

impl PumpHandle {
    /// Start the converter on the blocking pool (libyuv at 4K is milliseconds; a tokio
    /// worker must not carry it).
    pub fn spawn(mut pump: LocalVideoPump) -> Self {
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot {
                next: None,
                spare: Vec::new(),
                closed: false,
            }),
            cv: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        tokio::task::spawn_blocking(move || loop {
            let frame = {
                let mut slot = worker.slot.lock().unwrap();
                while slot.next.is_none() && !slot.closed {
                    slot = worker.cv.wait(slot).unwrap();
                }
                if slot.closed {
                    return;
                }
                slot.next.take()
            };
            let Some(frame) = frame else {
                continue;
            };
            match &frame {
                FrameIn::Bgrx {
                    width,
                    height,
                    stride,
                    timestamp_us,
                    bytes,
                } => pump.push(*width, *height, *stride, *timestamp_us, bytes),
                FrameIn::I420 {
                    width,
                    height,
                    timestamp_us,
                    planes,
                } => pump.push_i420(*width, *height, *timestamp_us, planes),
            }
            let mut slot = worker.slot.lock().unwrap();
            // Two is enough: one being read, one being converted.
            if slot.spare.len() < 2 {
                slot.spare.push(frame.into_buffer());
            }
        });
        Self { shared }
    }

    /// Hand the converter a frame; one still waiting is replaced (its buffer is kept).
    pub fn offer(&self, frame: FrameIn) {
        let mut slot = self.shared.slot.lock().unwrap();
        if let Some(stale) = slot.next.replace(frame) {
            if slot.spare.len() < 2 {
                slot.spare.push(stale.into_buffer());
            }
        }
        self.shared.cv.notify_one();
    }

    /// A buffer the converter is done with, for `read_msg_pooled`.
    pub fn take_spare(&self) -> Option<Vec<u8>> {
        self.shared.slot.lock().unwrap().spare.pop()
    }
}

impl Drop for PumpHandle {
    fn drop(&mut self) {
        let mut slot = self.shared.slot.lock().unwrap();
        slot.closed = true;
        slot.next = None;
        self.shared.cv.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_plane_handles_tight_and_padded_destinations() {
        let src = [1u8, 2, 3, 4, 5, 6];
        let mut tight = [0u8; 6];
        copy_plane(&src, 3, &mut tight, 3);
        assert_eq!(tight, src);
        let mut padded = [0u8; 8];
        copy_plane(&src, 3, &mut padded, 4);
        assert_eq!(padded, [1, 2, 3, 0, 4, 5, 6, 0]);
    }

    #[tokio::test]
    async fn offers_coalesce_to_the_newest_and_buffers_come_back() {
        let (tx, rx) = tokio::sync::watch::channel(super::super::wants::WantState::Hidden);
        let pump = LocalVideoPump::new(
            None,
            ShareLimits {
                max_fps: 60,
                max_height: None,
            },
            PreviewMirror::new(rx),
            "test",
            Arc::new(crate::media_server::FrameFanout::new()),
        );
        let handle = PumpHandle::spawn(pump);
        // Fill the slot twice before the converter can run: the first buffer is spare at once.
        handle.offer(FrameIn::I420 {
            width: 4,
            height: 4,
            timestamp_us: 1,
            planes: vec![0u8; 24],
        });
        handle.offer(FrameIn::I420 {
            width: 4,
            height: 4,
            timestamp_us: 2,
            planes: vec![0u8; 24],
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut back = 0;
        while back < 2 && std::time::Instant::now() < deadline {
            if handle.take_spare().is_some() {
                back += 1;
            } else {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
        assert_eq!(
            back, 2,
            "both buffers return: the replaced one and the converted one"
        );
        drop(tx);
    }
}
