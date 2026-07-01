//! pollis-capture-proto
//!
//! The single shared definition of the capture helper Unix-socket wire
//! protocol — both screen capture and webcam capture. Both per-platform
//! helper subprocesses (`pollis-capture-linux`, `pollis-capture-macos`)
//! encode frames with this crate; `pollis-core`'s main-process reader
//! decodes them with it. The helper is launched in either screen or
//! camera mode; the two modes share the Format + Frame messages and
//! differ only in their enumeration/selection handshake.
//!
//! This crate exists so the wire bytes have exactly one home. It was
//! factored out of the original hand-rolled encode/decode that lived in
//! `pollis-capture-linux/src/linux.rs` and
//! `pollis-core/src/commands/screenshare.rs` — the byte layout is
//! **unchanged**; only its location moved.
//!
//! Wire protocol (all integers little-endian):
//!
//!   message := [ u8 type ][ u32 payload_len ][ payload ]
//!
//!   type 0x01  Format
//!     payload := [ u32 width ][ u32 height ]
//!     Sent once when the source format is negotiated/known.
//!
//!   type 0x02  Frame
//!     payload := [ u32 width ][ u32 height ][ u32 stride ]
//!                [ i64 timestamp_us ][ BGRx bytes ... ]
//!     Pixel format is BGRx (4 bpp), top-down. The parent does the
//!     I420 conversion + LiveKit publish.
//!
//!   type 0x03  Sources (helper → parent)
//!     payload := utf-8 JSON `SourceList`
//!     Sent once after the helper has enumerated the OS's shareable
//!     content (macOS only today — built around `SCShareableContent`).
//!     Linux uses the system portal and never sends this. The parent
//!     renders the list in its own picker UI, then replies with Select.
//!
//!   type 0x04  Select (parent → helper)
//!     payload := utf-8 JSON `Selection`
//!     The parent's response to Sources. Carries the chosen
//!     display/window/app identifier; the helper builds an
//!     `SCContentFilter` from it and proceeds to Format → Frame.
//!
//!   type 0x05  Cameras (helper → parent)
//!     payload := utf-8 JSON `CameraList`
//!     Sent once in camera mode after the helper enumerates the OS's
//!     video-capture devices. The parent renders them in its own picker
//!     (it lists every device the OS reports — no virtual-camera
//!     filtering, matching Discord/Zoom), then replies with SelectCamera.
//!
//!   type 0x06  SelectCamera (parent → helper)
//!     payload := utf-8 JSON `CameraSelection`
//!     The parent's response to Cameras. Carries the opaque per-platform
//!     device id (macOS `AVCaptureDevice.uniqueID`, Linux V4L2 node path,
//!     Windows MF symbolic link) — a String, unlike the u32 ids screen
//!     sources use. The helper opens that device and proceeds to Format →
//!     Frame. Camera frames reuse the Format + Frame messages unchanged:
//!     the helper delivers BGRA (alpha ignored) exactly like the screen
//!     path, so the parent's I420 conversion + LiveKit publish is shared.
//!
//!   type 0xFF  Error
//!     payload := utf-8 message
//!
//! Lifecycle on macOS (screen): helper connects → Sources → (parent reads,
//! shows picker) → Select → Format → Frame ... until the parent
//! closes the socket.
//! Lifecycle on Linux (screen): helper connects → Format → Frame ... (no
//! enumeration round-trip; portal owns the picker).
//! Lifecycle in camera mode (all platforms): helper connects → Cameras →
//! (parent reads, shows picker / auto-picks) → SelectCamera → Format →
//! Frame ... until the parent closes the socket.
//! The parent stops capture by closing the socket; the helper observes
//! EPIPE on next write or EOF on read and exits.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Format announcement.
pub const MSG_FORMAT: u8 = 0x01;
/// A single BGRx frame.
pub const MSG_FRAME: u8 = 0x02;
/// Enumerated shareable sources, helper → parent. JSON payload.
pub const MSG_SOURCES: u8 = 0x03;
/// User's pick from the in-app picker, parent → helper. JSON payload.
pub const MSG_SELECT: u8 = 0x04;
/// Enumerated video-capture devices, helper → parent. JSON payload.
pub const MSG_CAMERAS: u8 = 0x05;
/// User's camera pick from the in-app picker, parent → helper. JSON payload.
pub const MSG_SELECT_CAMERA: u8 = 0x06;
/// A fatal error from the helper, carrying a human-readable utf-8 string.
pub const MSG_ERROR: u8 = 0xFF;

/// Hard cap on a single message payload. An 8K BGRx frame is ~127 MB;
/// anything past 32 MB is treated as a desync rather than a real frame.
/// Kept here so encoder and decoder share one definition.
pub const MAX_PAYLOAD_LEN: usize = 32 * 1024 * 1024;

/// A decoded protocol message.
#[derive(Debug)]
pub enum CaptureMsg {
    Format {
        width: u32,
        height: u32,
    },
    Frame {
        width: u32,
        height: u32,
        stride: u32,
        timestamp_us: i64,
        bgrx: Vec<u8>,
    },
    Sources(SourceList),
    Select(Selection),
    Cameras(CameraList),
    SelectCamera(CameraSelection),
    Error {
        message: String,
    },
}

/// A capturable display (whole monitor).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplaySource {
    /// macOS `CGDirectDisplayID` (helper path) or 0-based Windows enum
    /// index. The parent passes it back verbatim in `Selection::Display`.
    pub id: u32,
    pub width: u32,
    pub height: u32,
    /// Friendly label like "Built-in Retina Display" — for picker UI.
    pub name: String,
    /// Base64 PNG data URL rendered as the picker tile preview. `None`
    /// where the source path doesn't ship thumbnails (the macOS capture
    /// helper). Skipped on wire when absent for forward-compat with
    /// helpers built against the older proto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_data_url: Option<String>,
}

/// A capturable on-screen window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSource {
    /// macOS `CGWindowID` (helper path) or 0-based Windows enum index.
    /// The parent passes it back verbatim in `Selection::Window`.
    pub id: u32,
    pub width: u32,
    pub height: u32,
    /// Window title. Often empty — the OS doesn't enforce one.
    pub title: String,
    /// The owning application's display name (e.g. "Safari"). Used as
    /// the primary label when `title` is empty.
    pub app_name: String,
    /// Bundle identifier where known (e.g. "com.apple.Safari"). May be
    /// empty for daemons / agent processes without a bundle. Always
    /// empty on Windows (no analog).
    pub bundle_id: String,
    /// Base64 PNG data URL rendered as the picker tile preview. `None`
    /// where the source path doesn't ship thumbnails (the macOS capture
    /// helper). Skipped on wire when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_data_url: Option<String>,
}

/// The enumeration result sent helper → parent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceList {
    pub displays: Vec<DisplaySource>,
    pub windows: Vec<WindowSource>,
}

/// What the user picked in the in-app picker. Parent → helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Selection {
    Display { id: u32 },
    Window { id: u32 },
}

/// A capturable video-capture device (webcam / capture card / virtual cam).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraSource {
    /// Opaque, stable per-platform device handle: macOS
    /// `AVCaptureDevice.uniqueID`, Linux V4L2 node path (e.g.
    /// `/dev/video0`), Windows MF symbolic link. A String, unlike the
    /// u32 ids `DisplaySource`/`WindowSource` use — camera handles are
    /// not small integers. The parent passes it back verbatim in
    /// `CameraSelection`.
    pub id: String,
    /// Friendly label like "FaceTime HD Camera" — for picker UI.
    pub name: String,
}

/// The camera enumeration result sent helper → parent. Lists every
/// device the OS reports; no virtual-camera filtering (matches the
/// Discord/Zoom convention — the parent shows them all).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraList {
    pub cameras: Vec<CameraSource>,
}

/// What the user picked in the camera picker. Parent → helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraSelection {
    /// The chosen `CameraSource::id`, echoed back verbatim.
    pub id: String,
}

// ── Encoding (helper side) ────────────────────────────────────────────────

/// Serialize a Format message to its exact wire bytes.
pub fn encode_format(width: u32, height: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 4 + 8);
    buf.push(MSG_FORMAT);
    buf.extend_from_slice(&8u32.to_le_bytes());
    buf.extend_from_slice(&width.to_le_bytes());
    buf.extend_from_slice(&height.to_le_bytes());
    buf
}

/// Serialize a Frame header (everything up to and excluding the BGRx
/// payload). Callers write this then write the BGRx bytes directly so a
/// large frame need not be copied into a second buffer.
pub fn encode_frame_header(
    width: u32,
    height: u32,
    stride: u32,
    timestamp_us: i64,
    bgrx_len: usize,
) -> Vec<u8> {
    let payload_len = (4 + 4 + 4 + 8 + bgrx_len) as u32;
    let mut header = Vec::with_capacity(1 + 4 + 4 + 4 + 4 + 8);
    header.push(MSG_FRAME);
    header.extend_from_slice(&payload_len.to_le_bytes());
    header.extend_from_slice(&width.to_le_bytes());
    header.extend_from_slice(&height.to_le_bytes());
    header.extend_from_slice(&stride.to_le_bytes());
    header.extend_from_slice(&timestamp_us.to_le_bytes());
    header
}

/// Serialize a Sources message (helper → parent).
pub fn encode_sources(list: &SourceList) -> Vec<u8> {
    let json = serde_json::to_vec(list).expect("SourceList serializes");
    let mut buf = Vec::with_capacity(1 + 4 + json.len());
    buf.push(MSG_SOURCES);
    buf.extend_from_slice(&(json.len() as u32).to_le_bytes());
    buf.extend_from_slice(&json);
    buf
}

/// Serialize a Select message (parent → helper).
pub fn encode_select(sel: &Selection) -> Vec<u8> {
    let json = serde_json::to_vec(sel).expect("Selection serializes");
    let mut buf = Vec::with_capacity(1 + 4 + json.len());
    buf.push(MSG_SELECT);
    buf.extend_from_slice(&(json.len() as u32).to_le_bytes());
    buf.extend_from_slice(&json);
    buf
}

/// Serialize a Cameras message (helper → parent).
pub fn encode_cameras(list: &CameraList) -> Vec<u8> {
    let json = serde_json::to_vec(list).expect("CameraList serializes");
    let mut buf = Vec::with_capacity(1 + 4 + json.len());
    buf.push(MSG_CAMERAS);
    buf.extend_from_slice(&(json.len() as u32).to_le_bytes());
    buf.extend_from_slice(&json);
    buf
}

/// Serialize a SelectCamera message (parent → helper).
pub fn encode_select_camera(sel: &CameraSelection) -> Vec<u8> {
    let json = serde_json::to_vec(sel).expect("CameraSelection serializes");
    let mut buf = Vec::with_capacity(1 + 4 + json.len());
    buf.push(MSG_SELECT_CAMERA);
    buf.extend_from_slice(&(json.len() as u32).to_le_bytes());
    buf.extend_from_slice(&json);
    buf
}

/// Serialize an Error message to its exact wire bytes.
pub fn encode_error(message: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 4 + message.len());
    buf.push(MSG_ERROR);
    buf.extend_from_slice(&(message.len() as u32).to_le_bytes());
    buf.extend_from_slice(message.as_bytes());
    buf
}

/// Write a complete message to an async writer. Convenience for helpers
/// that already have the full frame buffer in hand.
pub async fn write_msg<W>(w: &mut W, msg: &CaptureMsg) -> std::io::Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    match msg {
        CaptureMsg::Format { width, height } => {
            w.write_all(&encode_format(*width, *height)).await
        }
        CaptureMsg::Frame {
            width,
            height,
            stride,
            timestamp_us,
            bgrx,
        } => {
            let header =
                encode_frame_header(*width, *height, *stride, *timestamp_us, bgrx.len());
            w.write_all(&header).await?;
            w.write_all(bgrx).await
        }
        CaptureMsg::Sources(list) => w.write_all(&encode_sources(list)).await,
        CaptureMsg::Select(sel) => w.write_all(&encode_select(sel)).await,
        CaptureMsg::Cameras(list) => w.write_all(&encode_cameras(list)).await,
        CaptureMsg::SelectCamera(sel) => w.write_all(&encode_select_camera(sel)).await,
        CaptureMsg::Error { message } => w.write_all(&encode_error(message)).await,
    }
}

// ── Decoding (parent side) ────────────────────────────────────────────────

/// Read one framed message from an async reader. Returns `Ok(None)` on a
/// clean EOF (parent closed the socket / helper exited). This is the
/// exact decode logic that used to live in `screenshare.rs`'s
/// `SocketReader::read_message`, byte-for-byte.
pub async fn read_msg<R>(r: &mut R) -> std::io::Result<Option<CaptureMsg>>
where
    R: AsyncReadExt + Unpin,
{
    let mut header = [0u8; 5];
    match r.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let msg_type = header[0];
    let payload_len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("oversized helper message: {payload_len}"),
        ));
    }
    match msg_type {
        MSG_FORMAT => {
            if payload_len != 8 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "format payload != 8",
                ));
            }
            let mut buf = [0u8; 8];
            r.read_exact(&mut buf).await?;
            Ok(Some(CaptureMsg::Format {
                width: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
                height: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            }))
        }
        MSG_FRAME => {
            if payload_len < 4 + 4 + 4 + 8 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "frame payload too short",
                ));
            }
            let mut head = [0u8; 4 + 4 + 4 + 8];
            r.read_exact(&mut head).await?;
            let width = u32::from_le_bytes(head[0..4].try_into().unwrap());
            let height = u32::from_le_bytes(head[4..8].try_into().unwrap());
            let stride = u32::from_le_bytes(head[8..12].try_into().unwrap());
            let timestamp_us = i64::from_le_bytes(head[12..20].try_into().unwrap());
            let body_len = payload_len - head.len();
            let mut bgrx = vec![0u8; body_len];
            r.read_exact(&mut bgrx).await?;
            Ok(Some(CaptureMsg::Frame {
                width,
                height,
                stride,
                timestamp_us,
                bgrx,
            }))
        }
        MSG_SOURCES => {
            let mut bytes = vec![0u8; payload_len];
            r.read_exact(&mut bytes).await?;
            let list: SourceList = serde_json::from_slice(&bytes).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("sources json: {e}"),
                )
            })?;
            Ok(Some(CaptureMsg::Sources(list)))
        }
        MSG_SELECT => {
            let mut bytes = vec![0u8; payload_len];
            r.read_exact(&mut bytes).await?;
            let sel: Selection = serde_json::from_slice(&bytes).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("select json: {e}"),
                )
            })?;
            Ok(Some(CaptureMsg::Select(sel)))
        }
        MSG_CAMERAS => {
            let mut bytes = vec![0u8; payload_len];
            r.read_exact(&mut bytes).await?;
            let list: CameraList = serde_json::from_slice(&bytes).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("cameras json: {e}"),
                )
            })?;
            Ok(Some(CaptureMsg::Cameras(list)))
        }
        MSG_SELECT_CAMERA => {
            let mut bytes = vec![0u8; payload_len];
            r.read_exact(&mut bytes).await?;
            let sel: CameraSelection = serde_json::from_slice(&bytes).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("select_camera json: {e}"),
                )
            })?;
            Ok(Some(CaptureMsg::SelectCamera(sel)))
        }
        MSG_ERROR => {
            let mut bytes = vec![0u8; payload_len];
            r.read_exact(&mut bytes).await?;
            let message = String::from_utf8_lossy(&bytes).into_owned();
            Ok(Some(CaptureMsg::Error { message }))
        }
        other => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unknown helper msg type: 0x{other:02x}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Round-trip every message kind through an in-memory duplex so the
    // exact wire bytes are exercised by encode -> decode.
    async fn roundtrip(msg: CaptureMsg) -> CaptureMsg {
        let (mut a, mut b) = tokio::io::duplex(1024 * 1024);
        write_msg(&mut a, &msg).await.unwrap();
        drop(a);
        read_msg(&mut b).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn format_roundtrip() {
        let m = roundtrip(CaptureMsg::Format {
            width: 1920,
            height: 1080,
        })
        .await;
        match m {
            CaptureMsg::Format { width, height } => {
                assert_eq!((width, height), (1920, 1080));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[tokio::test]
    async fn frame_roundtrip() {
        let bgrx = vec![0xABu8; 64 * 4];
        let m = roundtrip(CaptureMsg::Frame {
            width: 8,
            height: 8,
            stride: 32,
            timestamp_us: 123_456_789,
            bgrx: bgrx.clone(),
        })
        .await;
        match m {
            CaptureMsg::Frame {
                width,
                height,
                stride,
                timestamp_us,
                bgrx: got,
            } => {
                assert_eq!((width, height, stride), (8, 8, 32));
                assert_eq!(timestamp_us, 123_456_789);
                assert_eq!(got, bgrx);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[tokio::test]
    async fn error_roundtrip() {
        let m = roundtrip(CaptureMsg::Error {
            message: "portal: no backend".into(),
        })
        .await;
        match m {
            CaptureMsg::Error { message } => {
                assert_eq!(message, "portal: no backend");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[tokio::test]
    async fn clean_eof_is_none() {
        let (a, mut b) = tokio::io::duplex(16);
        drop(a);
        assert!(read_msg(&mut b).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn sources_roundtrip() {
        let m = roundtrip(CaptureMsg::Sources(SourceList {
            displays: vec![DisplaySource {
                id: 1,
                width: 3024,
                height: 1964,
                name: "Built-in Retina Display".into(),
                thumbnail_data_url: None,
            }],
            windows: vec![WindowSource {
                id: 42,
                width: 1280,
                height: 720,
                title: "claude-code — ghostty".into(),
                app_name: "Ghostty".into(),
                bundle_id: "com.mitchellh.ghostty".into(),
                thumbnail_data_url: None,
            }],
        }))
        .await;
        match m {
            CaptureMsg::Sources(list) => {
                assert_eq!(list.displays.len(), 1);
                assert_eq!(list.displays[0].id, 1);
                assert_eq!(list.windows.len(), 1);
                assert_eq!(list.windows[0].title, "claude-code — ghostty");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[tokio::test]
    async fn select_roundtrip() {
        match roundtrip(CaptureMsg::Select(Selection::Display { id: 7 })).await {
            CaptureMsg::Select(Selection::Display { id }) => assert_eq!(id, 7),
            _ => panic!("wrong variant"),
        }
        match roundtrip(CaptureMsg::Select(Selection::Window { id: 13 })).await {
            CaptureMsg::Select(Selection::Window { id }) => assert_eq!(id, 13),
            _ => panic!("wrong variant"),
        }
    }

    #[tokio::test]
    async fn cameras_roundtrip() {
        let m = roundtrip(CaptureMsg::Cameras(CameraList {
            cameras: vec![
                CameraSource {
                    id: "0x1420000005ac8600".into(),
                    name: "FaceTime HD Camera".into(),
                },
                CameraSource {
                    id: "/dev/video0".into(),
                    name: "Logitech BRIO".into(),
                },
            ],
        }))
        .await;
        match m {
            CaptureMsg::Cameras(list) => {
                assert_eq!(list.cameras.len(), 2);
                assert_eq!(list.cameras[0].name, "FaceTime HD Camera");
                assert_eq!(list.cameras[1].id, "/dev/video0");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[tokio::test]
    async fn select_camera_roundtrip() {
        match roundtrip(CaptureMsg::SelectCamera(CameraSelection {
            id: "/dev/video2".into(),
        }))
        .await
        {
            CaptureMsg::SelectCamera(sel) => assert_eq!(sel.id, "/dev/video2"),
            _ => panic!("wrong variant"),
        }
    }

    // The exact opcode bytes are load-bearing across three crates;
    // pin them so an accidental renumber is caught.
    #[test]
    fn opcodes_are_stable() {
        assert_eq!(MSG_FORMAT, 0x01);
        assert_eq!(MSG_FRAME, 0x02);
        assert_eq!(MSG_SOURCES, 0x03);
        assert_eq!(MSG_SELECT, 0x04);
        assert_eq!(MSG_CAMERAS, 0x05);
        assert_eq!(MSG_SELECT_CAMERA, 0x06);
        assert_eq!(MSG_ERROR, 0xFF);
    }
}
