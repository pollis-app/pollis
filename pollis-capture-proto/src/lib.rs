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
//!   type 0x07  AudioFormat (helper -> parent)
//!     payload := [ u32 sample_rate ][ u32 channels ]
//!     Sent once when a shared-audio source is negotiated. Its ARRIVAL
//!     is the signal that audio capture actually succeeded — the parent
//!     publishes the second LiveKit track only after seeing it.
//!
//!   type 0x08  AudioFrame (helper -> parent)
//!     payload := [ u32 sample_rate ][ u32 channels ][ i64 timestamp_us ]
//!                [ i16 interleaved PCM ... ]
//!     Signed 16-bit interleaved PCM at the announced rate/channel count.
//!     The parent downmixes to mono and resamples to 48 kHz
//!     (`screenshare::audio::normalize`) so all three platforms converge
//!     on one publish path.
//!
//!   type 0xFF  Error
//!     payload := utf-8 message
//!     A message prefixed `audio:` is NON-FATAL: shared-audio capture
//!     failed but video is unaffected, and the parent keeps the share
//!     running. Every other prefix ends the capture.
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

use serde::de::DeserializeOwned;
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
/// Shared-audio format announcement, helper -> parent.
pub const MSG_AUDIO_FORMAT: u8 = 0x07;
/// A block of interleaved s16 shared audio, helper -> parent.
pub const MSG_AUDIO_FRAME: u8 = 0x08;
/// An error from the helper, carrying a human-readable utf-8 string.
/// Fatal unless prefixed `audio:` — see the wire-protocol table above.
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
    AudioFormat {
        sample_rate: u32,
        channels: u32,
    },
    AudioFrame {
        sample_rate: u32,
        channels: u32,
        timestamp_us: i64,
        /// Interleaved signed 16-bit PCM, `channels` samples per frame.
        pcm: Vec<i16>,
    },
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
///
/// `with_audio` rides on the selection rather than on the helper's
/// command line because the macOS helper is spawned during
/// `enumerate_screen_sources` — before the user has seen the picker, and
/// so before the audio toggle has been read. Linux has no Select round
/// trip at all (the portal dialog is the picker), so its helper takes the
/// same decision as an `--audio` flag at spawn time instead. Defaulted so
/// a Select from an older frontend still deserializes as video-only.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Selection {
    Display {
        id: u32,
        #[serde(default)]
        with_audio: bool,
    },
    Window {
        id: u32,
        #[serde(default)]
        with_audio: bool,
    },
}

impl Selection {
    /// Whether the user asked for the source's audio alongside its video.
    pub fn with_audio(&self) -> bool {
        match self {
            Self::Display { with_audio, .. } | Self::Window { with_audio, .. } => *with_audio,
        }
    }
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

/// Wall-clock microseconds since the Unix epoch — the `timestamp_us` every
/// Frame and AudioFrame carries. Lives here so all capture backends stamp
/// from one clock; a pre-epoch clock yields 0 rather than panicking.
pub fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

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

/// Frame a JSON-payload message. Every JSON message on this protocol has
/// the same shape — opcode, u32 length, utf-8 JSON — so the four of them
/// share one encoder. Serialization of these types cannot fail (plain
/// structs of owned primitives), hence the `expect`.
fn encode_json<T: Serialize>(msg_type: u8, value: &T) -> Vec<u8> {
    let json = serde_json::to_vec(value).expect("capture proto message serializes");
    let mut buf = Vec::with_capacity(1 + 4 + json.len());
    buf.push(msg_type);
    buf.extend_from_slice(&(json.len() as u32).to_le_bytes());
    buf.extend_from_slice(&json);
    buf
}

/// Serialize a Sources message (helper → parent).
pub fn encode_sources(list: &SourceList) -> Vec<u8> {
    encode_json(MSG_SOURCES, list)
}

/// Serialize a Select message (parent → helper).
pub fn encode_select(sel: &Selection) -> Vec<u8> {
    encode_json(MSG_SELECT, sel)
}

/// Serialize a Cameras message (helper → parent).
pub fn encode_cameras(list: &CameraList) -> Vec<u8> {
    encode_json(MSG_CAMERAS, list)
}

/// Serialize a SelectCamera message (parent → helper).
pub fn encode_select_camera(sel: &CameraSelection) -> Vec<u8> {
    encode_json(MSG_SELECT_CAMERA, sel)
}

/// Serialize an AudioFormat message (helper → parent).
pub fn encode_audio_format(sample_rate: u32, channels: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 4 + 8);
    buf.push(MSG_AUDIO_FORMAT);
    buf.extend_from_slice(&8u32.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&channels.to_le_bytes());
    buf
}

/// Serialize a complete AudioFrame message (helper → parent). Audio
/// blocks are small (a 20 ms stereo block at 48 kHz is 3.8 KB), so unlike
/// video frames there is nothing to gain from a split header + payload
/// write — one buffer keeps the call sites simple.
pub fn encode_audio_frame(
    sample_rate: u32,
    channels: u32,
    timestamp_us: i64,
    pcm: &[i16],
) -> Vec<u8> {
    let payload_len = (4 + 4 + 8 + pcm.len() * 2) as u32;
    let mut buf = Vec::with_capacity(1 + 4 + payload_len as usize);
    buf.push(MSG_AUDIO_FRAME);
    buf.extend_from_slice(&payload_len.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&timestamp_us.to_le_bytes());
    for s in pcm {
        buf.extend_from_slice(&s.to_le_bytes());
    }
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
        CaptureMsg::AudioFormat {
            sample_rate,
            channels,
        } => w.write_all(&encode_audio_format(*sample_rate, *channels)).await,
        CaptureMsg::AudioFrame {
            sample_rate,
            channels,
            timestamp_us,
            pcm,
        } => {
            w.write_all(&encode_audio_frame(*sample_rate, *channels, *timestamp_us, pcm))
                .await
        }
        CaptureMsg::Error { message } => w.write_all(&encode_error(message)).await,
    }
}

// ── Decoding (parent side) ────────────────────────────────────────────────

/// Every decode failure on this protocol is a desync or a corrupt payload,
/// which is `InvalidData` in every case.
fn invalid_data(msg: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg)
}

/// Read exactly `len` payload bytes into a fresh buffer.
async fn read_payload<R>(r: &mut R, len: usize) -> std::io::Result<Vec<u8>>
where
    R: AsyncReadExt + Unpin,
{
    let mut bytes = vec![0u8; len];
    r.read_exact(&mut bytes).await?;
    Ok(bytes)
}

/// Read a JSON payload and deserialize it. `what` names the message so a
/// parse failure says which one desynced.
async fn read_json<T, R>(r: &mut R, len: usize, what: &str) -> std::io::Result<T>
where
    T: DeserializeOwned,
    R: AsyncReadExt + Unpin,
{
    let bytes = read_payload(r, len).await?;
    serde_json::from_slice(&bytes).map_err(|e| invalid_data(format!("{what} json: {e}")))
}

/// Read the two-u32 payload that Format and AudioFormat share.
async fn read_pair_u32<R>(r: &mut R, len: usize, what: &str) -> std::io::Result<(u32, u32)>
where
    R: AsyncReadExt + Unpin,
{
    if len != 8 {
        return Err(invalid_data(format!("{what} payload != 8")));
    }
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf).await?;
    Ok((
        u32::from_le_bytes(buf[0..4].try_into().unwrap()),
        u32::from_le_bytes(buf[4..8].try_into().unwrap()),
    ))
}

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
        return Err(invalid_data(format!(
            "oversized helper message: {payload_len}"
        )));
    }
    match msg_type {
        MSG_FORMAT => {
            let (width, height) = read_pair_u32(r, payload_len, "format").await?;
            Ok(Some(CaptureMsg::Format { width, height }))
        }
        MSG_FRAME => {
            if payload_len < 4 + 4 + 4 + 8 {
                return Err(invalid_data("frame payload too short"));
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
        MSG_SOURCES => Ok(Some(CaptureMsg::Sources(
            read_json(r, payload_len, "sources").await?,
        ))),
        MSG_SELECT => Ok(Some(CaptureMsg::Select(
            read_json(r, payload_len, "select").await?,
        ))),
        MSG_CAMERAS => Ok(Some(CaptureMsg::Cameras(
            read_json(r, payload_len, "cameras").await?,
        ))),
        MSG_SELECT_CAMERA => Ok(Some(CaptureMsg::SelectCamera(
            read_json(r, payload_len, "select_camera").await?,
        ))),
        MSG_AUDIO_FORMAT => {
            let (sample_rate, channels) = read_pair_u32(r, payload_len, "audio format").await?;
            Ok(Some(CaptureMsg::AudioFormat {
                sample_rate,
                channels,
            }))
        }
        MSG_AUDIO_FRAME => {
            const HEAD: usize = 4 + 4 + 8;
            if payload_len < HEAD {
                return Err(invalid_data("audio frame payload too short"));
            }
            let mut head = [0u8; HEAD];
            r.read_exact(&mut head).await?;
            let sample_rate = u32::from_le_bytes(head[0..4].try_into().unwrap());
            let channels = u32::from_le_bytes(head[4..8].try_into().unwrap());
            let timestamp_us = i64::from_le_bytes(head[8..16].try_into().unwrap());
            let body_len = payload_len - HEAD;
            // A half sample on the wire is a desync, not a short frame.
            if body_len % 2 != 0 {
                return Err(invalid_data(
                    "audio frame payload is not a whole number of s16 samples",
                ));
            }
            let bytes = read_payload(r, body_len).await?;
            let pcm: Vec<i16> = bytes
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();
            Ok(Some(CaptureMsg::AudioFrame {
                sample_rate,
                channels,
                timestamp_us,
                pcm,
            }))
        }
        MSG_ERROR => {
            let bytes = read_payload(r, payload_len).await?;
            let message = String::from_utf8_lossy(&bytes).into_owned();
            Ok(Some(CaptureMsg::Error { message }))
        }
        other => Err(invalid_data(format!(
            "unknown helper msg type: 0x{other:02x}"
        ))),
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
        match roundtrip(CaptureMsg::Select(Selection::Display {
            id: 7,
            with_audio: false,
        }))
        .await
        {
            CaptureMsg::Select(Selection::Display { id, with_audio }) => {
                assert_eq!(id, 7);
                assert!(!with_audio);
            }
            _ => panic!("wrong variant"),
        }
        match roundtrip(CaptureMsg::Select(Selection::Window {
            id: 13,
            with_audio: true,
        }))
        .await
        {
            CaptureMsg::Select(Selection::Window { id, with_audio }) => {
                assert_eq!(id, 13);
                assert!(with_audio);
            }
            _ => panic!("wrong variant"),
        }
    }

    // A Select minted by a frontend built before shared audio existed
    // carries no `with_audio` key at all. It must deserialize as a
    // video-only share rather than failing the handshake — a helper and a
    // renderer can be a release apart.
    #[test]
    fn select_without_with_audio_defaults_to_video_only() {
        let sel: Selection = serde_json::from_str(r#"{"kind":"display","id":3}"#).unwrap();
        assert!(!sel.with_audio());
        assert!(matches!(sel, Selection::Display { id: 3, .. }));
    }

    #[tokio::test]
    async fn audio_format_roundtrip() {
        match roundtrip(CaptureMsg::AudioFormat {
            sample_rate: 48_000,
            channels: 2,
        })
        .await
        {
            CaptureMsg::AudioFormat {
                sample_rate,
                channels,
            } => assert_eq!((sample_rate, channels), (48_000, 2)),
            _ => panic!("wrong variant"),
        }
    }

    #[tokio::test]
    async fn audio_frame_roundtrip() {
        // Includes both signs and both extremes so a byte-order or
        // sign-extension slip in the s16 pack/unpack shows up.
        let pcm: Vec<i16> = vec![0, 1, -1, i16::MAX, i16::MIN, 12_345, -12_345, 256];
        match roundtrip(CaptureMsg::AudioFrame {
            sample_rate: 44_100,
            channels: 2,
            timestamp_us: -42,
            pcm: pcm.clone(),
        })
        .await
        {
            CaptureMsg::AudioFrame {
                sample_rate,
                channels,
                timestamp_us,
                pcm: got,
            } => {
                assert_eq!((sample_rate, channels), (44_100, 2));
                assert_eq!(timestamp_us, -42);
                assert_eq!(got, pcm);
            }
            _ => panic!("wrong variant"),
        }
    }

    // An empty block is legal on the wire (a capture backend can hand us a
    // zero-length period); it must decode as an empty frame, not an error.
    #[tokio::test]
    async fn empty_audio_frame_roundtrips() {
        match roundtrip(CaptureMsg::AudioFrame {
            sample_rate: 48_000,
            channels: 1,
            timestamp_us: 0,
            pcm: Vec::new(),
        })
        .await
        {
            CaptureMsg::AudioFrame { pcm, .. } => assert!(pcm.is_empty()),
            _ => panic!("wrong variant"),
        }
    }

    // An odd payload length cannot be a whole number of s16 samples, so it
    // is a stream desync. Truncating silently would emit a half-sample of
    // noise on every subsequent block.
    #[tokio::test]
    async fn odd_length_audio_payload_is_rejected() {
        let mut bytes = encode_audio_frame(48_000, 1, 0, &[1, 2, 3]);
        // Drop one trailing byte and fix the declared length to match.
        bytes.pop();
        let payload_len = (bytes.len() - 5) as u32;
        bytes[1..5].copy_from_slice(&payload_len.to_le_bytes());
        let (mut a, mut b) = tokio::io::duplex(1024);
        tokio::io::AsyncWriteExt::write_all(&mut a, &bytes)
            .await
            .unwrap();
        drop(a);
        let err = read_msg(&mut b).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
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
        assert_eq!(MSG_AUDIO_FORMAT, 0x07);
        assert_eq!(MSG_AUDIO_FRAME, 0x08);
        assert_eq!(MSG_ERROR, 0xFF);
    }
}
