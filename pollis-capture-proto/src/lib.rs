//! pollis-capture-proto: the single definition of the capture helpers'
//! Unix-socket wire protocol (screen and camera), shared by the helpers and
//! `pollis-core`'s reader.
//!
//! Wire protocol (all integers little-endian):
//!
//!   message := [ u8 type ][ u32 payload_len ][ payload ]
//!
//!   type 0x01  Format
//!     payload := [ u32 width ][ u32 height ]
//!
//!   type 0x02  Frame
//!     payload := [ u32 width ][ u32 height ][ u32 stride ]
//!                [ i64 timestamp_us ][ BGRx bytes ... ]
//!     Pixel format is BGRx (4 bpp), top-down.
//!
//!   type 0x03  Sources (helper → parent)
//!     payload := utf-8 JSON `SourceList`
//!     macOS only; Linux uses the system portal. The parent shows its own
//!     picker, then replies with Select.
//!
//!   type 0x04  Select (parent → helper)
//!     payload := utf-8 JSON `Selection`
//!     The chosen source; the helper then proceeds to Format → Frame.
//!
//!   type 0x05  Cameras (helper → parent)
//!     payload := utf-8 JSON `CameraList`
//!     Every device the OS reports; the parent replies with SelectCamera.
//!
//!   type 0x06  SelectCamera (parent → helper)
//!     payload := utf-8 JSON `CameraSelection`
//!     The opaque per-platform device id. Camera frames then reuse Format +
//!     Frame unchanged (BGRA, alpha ignored).
//!
//!   type 0x07  AudioFormat (helper -> parent)
//!     payload := [ u32 sample_rate ][ u32 channels ]
//!     Its arrival is the signal that audio capture succeeded; the parent
//!     publishes the audio track only after seeing it.
//!
//!   type 0x08  AudioFrame (helper -> parent)
//!     payload := [ u32 sample_rate ][ u32 channels ][ i64 timestamp_us ]
//!                [ i16 interleaved PCM ... ]
//!     Signed 16-bit interleaved PCM at the announced rate/channel count.
//!
//!   type 0xFF  Error
//!     payload := utf-8 message
//!     A message prefixed `audio:` is non-fatal (video continues); every
//!     other error ends the capture.
//!
//! Lifecycles: macOS screen: Sources → Select → Format → Frame...; Linux
//! screen: Format → Frame...; camera: Cameras → SelectCamera → Format →
//! Frame.... The parent stops capture by closing the socket.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const MSG_FORMAT: u8 = 0x01;
pub const MSG_FRAME: u8 = 0x02;
pub const MSG_SOURCES: u8 = 0x03;
pub const MSG_SELECT: u8 = 0x04;
pub const MSG_CAMERAS: u8 = 0x05;
pub const MSG_SELECT_CAMERA: u8 = 0x06;
pub const MSG_AUDIO_FORMAT: u8 = 0x07;
pub const MSG_AUDIO_FRAME: u8 = 0x08;
/// Fatal unless prefixed `audio:`.
pub const MSG_ERROR: u8 = 0xFF;

/// Hard cap on a single payload; anything larger is treated as a desync.
pub const MAX_PAYLOAD_LEN: usize = 32 * 1024 * 1024;

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
    /// macOS `CGDirectDisplayID` or Windows enum index, echoed in `Selection`.
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub name: String,
    /// Base64 PNG data URL for the picker tile. Skipped when absent, for
    /// compatibility with older helpers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_data_url: Option<String>,
}

/// A capturable on-screen window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSource {
    /// macOS `CGWindowID` or Windows enum index, echoed in `Selection`.
    pub id: u32,
    pub width: u32,
    pub height: u32,
    /// Often empty: the OS doesn't enforce one.
    pub title: String,
    pub app_name: String,
    /// Empty without a bundle, and always on Windows.
    pub bundle_id: String,
    /// Base64 PNG data URL for the picker tile. Skipped when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_data_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceList {
    pub displays: Vec<DisplaySource>,
    pub windows: Vec<WindowSource>,
}

/// The in-app picker's choice. Parent → helper.
///
/// `with_audio` rides here, not on the command line, because the macOS helper
/// is spawned before the picker is shown (Linux uses `--audio` instead).
/// Defaulted so an older frontend's Select is video-only.
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
    pub fn with_audio(&self) -> bool {
        match self {
            Self::Display { with_audio, .. } | Self::Window { with_audio, .. } => *with_audio,
        }
    }
}

/// A capturable video-capture device (webcam / capture card / virtual cam).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraSource {
    /// Opaque, stable per-platform handle (macOS `AVCaptureDevice.uniqueID`,
    /// V4L2 node path, Windows MF symbolic link), echoed in `CameraSelection`.
    pub id: String,
    pub name: String,
}

/// Every device the OS reports; no virtual-camera filtering (as Discord/Zoom).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraList {
    pub cameras: Vec<CameraSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraSelection {
    pub id: String,
}

/// Wall-clock microseconds since the epoch, the one clock every backend stamps
/// `timestamp_us` from. A pre-epoch clock yields 0.
pub fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

pub fn encode_format(width: u32, height: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 4 + 8);
    buf.push(MSG_FORMAT);
    buf.extend_from_slice(&8u32.to_le_bytes());
    buf.extend_from_slice(&width.to_le_bytes());
    buf.extend_from_slice(&height.to_le_bytes());
    buf
}

/// A Frame header without the BGRx payload, so callers write the pixels
/// directly instead of copying them into a second buffer.
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

/// Serialization of these plain types cannot fail, hence the `expect`.
fn encode_json<T: Serialize>(msg_type: u8, value: &T) -> Vec<u8> {
    let json = serde_json::to_vec(value).expect("capture proto message serializes");
    let mut buf = Vec::with_capacity(1 + 4 + json.len());
    buf.push(msg_type);
    buf.extend_from_slice(&(json.len() as u32).to_le_bytes());
    buf.extend_from_slice(&json);
    buf
}

pub fn encode_sources(list: &SourceList) -> Vec<u8> {
    encode_json(MSG_SOURCES, list)
}

pub fn encode_select(sel: &Selection) -> Vec<u8> {
    encode_json(MSG_SELECT, sel)
}

pub fn encode_cameras(list: &CameraList) -> Vec<u8> {
    encode_json(MSG_CAMERAS, list)
}

pub fn encode_select_camera(sel: &CameraSelection) -> Vec<u8> {
    encode_json(MSG_SELECT_CAMERA, sel)
}

pub fn encode_audio_format(sample_rate: u32, channels: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 4 + 8);
    buf.push(MSG_AUDIO_FORMAT);
    buf.extend_from_slice(&8u32.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&channels.to_le_bytes());
    buf
}

/// A complete AudioFrame in one buffer; audio blocks are small (~4 KB).
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

pub fn encode_error(message: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 4 + message.len());
    buf.push(MSG_ERROR);
    buf.extend_from_slice(&(message.len() as u32).to_le_bytes());
    buf.extend_from_slice(message.as_bytes());
    buf
}

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

fn invalid_data(msg: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg)
}

async fn read_payload<R>(r: &mut R, len: usize) -> std::io::Result<Vec<u8>>
where
    R: AsyncReadExt + Unpin,
{
    let mut bytes = vec![0u8; len];
    r.read_exact(&mut bytes).await?;
    Ok(bytes)
}

/// `what` names the message so a parse failure says which one desynced.
async fn read_json<T, R>(r: &mut R, len: usize, what: &str) -> std::io::Result<T>
where
    T: DeserializeOwned,
    R: AsyncReadExt + Unpin,
{
    let bytes = read_payload(r, len).await?;
    serde_json::from_slice(&bytes).map_err(|e| invalid_data(format!("{what} json: {e}")))
}

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

/// Read one framed message; `Ok(None)` on a clean EOF.
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

    // A pre-audio frontend's Select has no `with_audio` key; it must still
    // deserialize, since helper and renderer can be a release apart.
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
        // Both signs and extremes, to catch byte-order or sign-extension slips.
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

    // A zero-length period is legal and decodes as an empty frame.
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

    // An odd payload length is a desync; truncating would add noise to every
    // later block.
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

    // Opcodes are shared wire across three crates.
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
