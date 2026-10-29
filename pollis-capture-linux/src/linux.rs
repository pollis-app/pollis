use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use pollis_capture_proto::{write_msg, CaptureMsg};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

#[derive(Parser, Debug)]
#[command(name = "pollis-capture-linux", version)]
struct Args {
    /// Path of the Unix socket that the parent (Pollis main) is
    /// listening on. We connect to it once at startup; closing this
    /// socket is the parent's signal to make us exit.
    #[arg(long)]
    socket: String,
    /// What to capture. `screen` (default) drives the portal/PipeWire or
    /// X11/SHM backend; `camera` drives V4L2. They share the Format/Frame
    /// wire protocol and differ only in enumeration/selection — and camera
    /// skips the session-type probe entirely (V4L2 is display-agnostic).
    #[arg(long, value_enum, default_value_t = Mode::Screen)]
    mode: Mode,
    /// Also capture the machine's audio output and stream it alongside the
    /// video. A spawn-time flag rather than a protocol message because the
    /// portal dialog is this helper's picker — there is no Select round
    /// trip to carry the decision on, unlike macOS. Ignored in camera mode.
    #[arg(long, default_value_t = false)]
    audio: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Mode {
    Screen,
    Camera,
}

/// Which capture backend the routing probe selected for this session.
///
/// This is decided ONCE at capture start. We route on *session type*,
/// not desktop-environment name: GNOME and KDE both ship X11 sessions
/// too, so a DE-name switch (issue #281's original misdiagnosis) would
/// mis-route them. Under XWayland an X11 client gets a private root, not
/// the composited screen, so X11 grab returns black on Wayland — the
/// two-backend split is mandatory, not an optimization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    /// Wayland session with a working xdg-desktop-portal ScreenCast
    /// backend + PipeWire. The original, unchanged path.
    Portal,
    /// X11 session (Xorg or an X11 session of GNOME/KDE, plus
    /// Cinnamon/MATE/XFCE which have no ScreenCast portal backend at
    /// all). New xcb/SHM/RandR path.
    X11,
    /// Wayland session with NO ScreenCast portal backend (e.g.
    /// Cinnamon-on-Wayland-ish setups, or a portal install missing the
    /// ScreenCast impl). Screen sharing is genuinely unavailable here —
    /// surfaced as a distinct, non-"denied" error.
    Unsupported,
}

/// Probe the session to decide the backend. Pure environment + bus
/// inspection; no UI, no user interaction.
async fn probe_backend() -> Backend {
    // $XDG_SESSION_TYPE, corroborated by $WAYLAND_DISPLAY / $DISPLAY for minimal sessions.
    let session_type = std::env::var("XDG_SESSION_TYPE")
        .unwrap_or_default()
        .to_lowercase();
    let has_wayland = std::env::var("WAYLAND_DISPLAY")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let has_x11 = std::env::var("DISPLAY")
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    let is_wayland = session_type == "wayland" || (session_type.is_empty() && has_wayland);
    let is_x11 = session_type == "x11" || (session_type.is_empty() && has_x11 && !has_wayland);

    if is_x11 && !is_wayland {
        // X11 grab works with or without a portal (Cinnamon/MATE/XFCE have none).
        eprintln!("[capture] probe: X11 session -> X11 backend");
        return Backend::X11;
    }

    // Wayland: only the portal can work — XWayland would hand X11 a private root.
    if portal_screencast_available().await {
        eprintln!("[capture] probe: Wayland + ScreenCast portal -> Portal backend");
        Backend::Portal
    } else {
        eprintln!(
            "[capture] probe: Wayland but no ScreenCast portal backend -> Unsupported"
        );
        Backend::Unsupported
    }
}

/// Is `org.freedesktop.portal.ScreenCast` actually available on the
/// session bus? A bare `xdg-desktop-portal` with only the gtk backend
/// (Cinnamon/MATE/XFCE default) does NOT implement ScreenCast — the
/// interface is absent and the portal call errors before any picker UI.
/// We detect that here so we can emit a precise "your DE has no
/// ScreenCast backend" error instead of collapsing it into "denied".
async fn portal_screencast_available() -> bool {
    use ashpd::desktop::screencast::Screencast;
    // Fails fast, with no UI, if the backend lacks ScreenCast.
    match Screencast::new().await {
        Ok(proxy) => {
            // Succeeds only if a real backend is wired up.
            match proxy.available_source_types().await {
                Ok(_) => true,
                Err(e) => {
                    eprintln!("[capture] ScreenCast available_source_types failed: {e}");
                    false
                }
            }
        }
        Err(e) => {
            eprintln!("[capture] ScreenCast proxy construction failed: {e}");
            false
        }
    }
}

pub fn run() -> Result<()> {
    let args = Args::parse();

    // PR_SET_PDEATHSIG: SIGTERM us if the parent dies, so no orphaned pipewire stream.
    unsafe {
        libc_pdeathsig(pdeath::SIGTERM);
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move { run_async(&args.socket, args.mode, args.audio).await })
}

async fn run_async(socket_path: &str, mode: Mode, audio: bool) -> Result<()> {
    eprintln!("[capture] connecting to parent socket {socket_path} (mode={mode:?})");
    let mut sock = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connect to {socket_path}"))?;
    eprintln!("[capture] connected");

    // V4L2 does not depend on the session type.
    if mode == Mode::Camera {
        return crate::camera::run_camera(&mut sock).await;
    }

    eprintln!("[capture] probing session backend");
    let backend = probe_backend().await;

    match backend {
        Backend::Portal => run_portal(&mut sock, audio).await,
        Backend::X11 => {
            run_x11(&mut sock, audio).await
        }
        Backend::Unsupported => {
            // Distinct from user-denial: the parent shows an unsupported-DE UI.
            let msg = "unsupported: Screen sharing requires xdg-desktop-portal \
                       with a ScreenCast backend; your desktop environment does \
                       not provide one.";
            eprintln!("[capture] {msg}");
            write_msg(&mut sock, &CaptureMsg::Error { message: msg.into() })
                .await
                .ok();
            Err(anyhow!("no ScreenCast portal backend"))
        }
    }
}

/// Drain the capture channel → parent socket until the capture side ends
/// or the parent goes away (any write error — EPIPE and friends). Shared
/// by the portal, X11 and camera paths, which all bridge a synchronous
/// capture thread to this one socket the same way. `tag` is the caller's
/// log prefix.
pub(crate) async fn drain_to_socket(
    sock: &mut UnixStream,
    rx: &mut mpsc::Receiver<CaptureMsg>,
    tag: &str,
) {
    while let Some(msg) = rx.recv().await {
        if let Err(e) = write_msg(sock, &msg).await {
            eprintln!("{tag} socket write error: {e} — exiting");
            break;
        }
    }
}

/// The original Wayland + xdg-desktop-portal + PipeWire path. Logic
/// unchanged from before the split — only the wire encode now goes
/// through the shared `pollis-capture-proto` crate instead of the
/// hand-rolled `write_msg`/`send_error` that used to live here.
async fn run_portal(sock: &mut UnixStream, audio: bool) -> Result<()> {
    eprintln!("[capture] opening portal (waits for user picker)");

    let (node_id, fd) = match open_portal().await {
        Ok(v) => {
            eprintln!("[capture] portal returned node_id={}", v.0);
            v
        }
        Err(PortalError::Cancelled) => {
            // `cancel:` prefix lets the parent swallow a cancelled picker silently.
            eprintln!("[capture] portal cancelled by user");
            write_msg(
                sock,
                &CaptureMsg::Error {
                    message: "cancel: user dismissed picker".into(),
                },
            )
            .await
            .ok();
            return Ok(());
        }
        Err(PortalError::Other(e)) => {
            eprintln!("[capture] portal error: {e}");
            write_msg(
                sock,
                &CaptureMsg::Error {
                    message: format!("portal: {e}"),
                },
            )
            .await
            .ok();
            return Err(e);
        }
    };

    // Last-frame-wins for video; deeper with audio, where a dropped block is an audible click.
    let (tx, mut rx) = mpsc::channel::<CaptureMsg>(if audio { 16 } else { 2 });
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);

    eprintln!("[capture] spawning pipewire video thread");
    let tx_for_video = tx.clone();
    let pw_thread = std::thread::Builder::new()
        .name("pollis-capture-pw".into())
        .spawn(move || {
            eprintln!("[capture/pw] thread entered");
            if let Err(e) = pw::run_pipewire(node_id, fd, tx_for_video, stop_for_thread) {
                eprintln!("[capture/pw] error: {e}");
            }
            eprintln!("[capture/pw] thread exiting");
        })
        .context("spawn pipewire thread")?;

    // Audio never fails the share: `audio:` errors downgrade to video-only.
    let audio_thread = audio.then(|| crate::audio::spawn(tx.clone(), Arc::clone(&stop)));
    drop(tx);

    drain_to_socket(sock, &mut rx, "[capture]").await;

    stop.store(true, Ordering::Relaxed);
    drop(pw_thread);
    drop(audio_thread);
    Ok(())
}

/// The X11 backend (issue #281). Spawns the synchronous xcb/SHM capture
/// loop on its own thread and drains its frames to the socket exactly
/// the way the portal path drains the pipewire thread.
async fn run_x11(sock: &mut UnixStream, audio: bool) -> Result<()> {
    eprintln!("[capture] starting X11 (xcb/SHM/RandR) backend");

    // Deeper with audio: a dropped audio block is an audible gap.
    let (tx, mut rx) = mpsc::channel::<CaptureMsg>(if audio { 16 } else { 2 });
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);

    let tx_for_video = tx.clone();
    let x11_thread = std::thread::Builder::new()
        .name("pollis-capture-x11".into())
        .spawn(move || {
            eprintln!("[capture/x11] thread entered");
            if let Err(e) = crate::x11::run_x11_capture(tx_for_video.clone(), stop_for_thread) {
                eprintln!("[capture/x11] error: {e}");
                let _ = tx_for_video.blocking_send(CaptureMsg::Error {
                    message: format!("x11: {e}"),
                });
            }
            eprintln!("[capture/x11] thread exiting");
        })
        .context("spawn x11 thread")?;

    let audio_thread = audio.then(|| crate::audio::spawn(tx.clone(), Arc::clone(&stop)));
    drop(tx);

    drain_to_socket(sock, &mut rx, "[capture]").await;

    stop.store(true, Ordering::Relaxed);
    drop(x11_thread);
    drop(audio_thread);
    Ok(())
}

/// Distinguishes "user cancelled the picker" from "everything else"
/// so the parent can swallow cancel silently. Without this, ashpd's
/// `Response::Cancelled` collapses into the same anyhow error path as a
/// genuine portal failure → the parent shows a misleading "permission
/// denied" toast for a perfectly normal Escape-press.
enum PortalError {
    Cancelled,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for PortalError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

async fn open_portal() -> std::result::Result<(u32, OwnedFd), PortalError> {
    use ashpd::desktop::{
        screencast::{CursorMode, Screencast, SourceType},
        PersistMode, ResponseError,
    };
    let proxy = Screencast::new()
        .await
        .map_err(|e| anyhow!("screencast portal: {e}"))?;
    let session = proxy
        .create_session()
        .await
        .map_err(|e| anyhow!("create session: {e}"))?;
    proxy
        .select_sources(
            &session,
            CursorMode::Embedded,
            SourceType::Monitor | SourceType::Window,
            false,
            None,
            PersistMode::DoNot,
        )
        .await
        .map_err(|e| anyhow!("select sources: {e}"))?;
    let request = proxy
        .start(&session, &ashpd::WindowIdentifier::default())
        .await
        .map_err(|e| anyhow!("portal start: {e}"))?;
    let response = match request.response() {
        Ok(r) => r,
        // Keep the structured cancel signal for the parent's error mapping.
        Err(ashpd::Error::Response(ResponseError::Cancelled)) => {
            return Err(PortalError::Cancelled);
        }
        Err(e) => return Err(PortalError::Other(anyhow!("portal response: {e}"))),
    };
    let stream = response
        .streams()
        .first()
        .ok_or_else(|| anyhow!("portal returned no streams"))?
        .to_owned();
    let fd = proxy
        .open_pipe_wire_remote(&session)
        .await
        .map_err(|e| anyhow!("open pw remote: {e}"))?;
    let node_id = stream.pipe_wire_node_id();
    // Leak: dropping the session silently kills the pipewire stream.
    std::mem::forget(session);
    std::mem::forget(proxy);
    Ok((node_id, fd))
}

mod pdeath {
    pub const SIGTERM: i32 = 15;
    pub const PR_SET_PDEATHSIG: i32 = 1;
    extern "C" {
        pub fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
    }
}

unsafe fn libc_pdeathsig(sig: i32) {
    let _ = pdeath::prctl(pdeath::PR_SET_PDEATHSIG, sig as u64, 0, 0, 0);
}

mod pw {
    use anyhow::Result;
    use pollis_capture_proto::{now_us, CaptureMsg};
    use std::os::fd::OwnedFd;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::sync::mpsc;

    pub fn run_pipewire(
        node_id: u32,
        fd: OwnedFd,
        tx: mpsc::Sender<CaptureMsg>,
        stop: Arc<AtomicBool>,
    ) -> Result<()> {
        use pipewire as pw;
        use pw::{properties::properties, spa};

        pw::init();
        let mainloop = pw::main_loop::MainLoopRc::new(None)?;
        let context = pw::context::ContextRc::new(&mainloop, None)?;
        // The portal fd is a full pipewire core fd, shared by video and audio.
        let core = context.connect_fd_rc(fd, None)?;

        struct Data {
            format: spa::param::video::VideoInfoRaw,
            announced: Option<(u32, u32)>,
        }
        let data = Data {
            format: Default::default(),
            announced: None,
        };

        let stream = pw::stream::StreamRc::new(
            core,
            "pollis-screenshare",
            properties! {
                *pw::keys::MEDIA_TYPE => "Video",
                *pw::keys::MEDIA_CATEGORY => "Capture",
                *pw::keys::MEDIA_ROLE => "Screen",
            },
        )?;

        let mainloop_for_quit = mainloop.clone();
        let stop_for_proc = Arc::clone(&stop);
        let tx_for_proc = tx.clone();
        let tx_for_format = tx;

        let _listener = stream
            .add_local_listener_with_user_data::<Data>(data)
            .state_changed(|_, _, old, new| {
                eprintln!("[capture/pw] state {:?} -> {:?}", old, new);
            })
            .param_changed(move |_, ud, id, param| {
                let Some(param) = param else { return; };
                if id != pw::spa::param::ParamType::Format.as_raw() {
                    return;
                }
                let Ok((mt, ms)) = pw::spa::param::format_utils::parse_format(param) else {
                    return;
                };
                if mt != pw::spa::param::format::MediaType::Video
                    || ms != pw::spa::param::format::MediaSubtype::Raw
                {
                    return;
                }
                ud.format.parse(param).ok();
                let w = ud.format.size().width;
                let h = ud.format.size().height;
                if w == 0 || h == 0 {
                    return;
                }
                if ud.announced != Some((w, h)) {
                    ud.announced = Some((w, h));
                    eprintln!(
                        "[capture/pw] format negotiated {:?} {}x{}",
                        ud.format.format(),
                        w,
                        h
                    );
                    let _ = tx_for_format.try_send(CaptureMsg::Format { width: w, height: h });
                }
            })
            .process(move |stream, ud| {
                if stop_for_proc.load(Ordering::Relaxed) {
                    mainloop_for_quit.quit();
                    return;
                }
                let Some(mut buffer) = stream.dequeue_buffer() else { return; };
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }
                let width = ud.format.size().width;
                let height = ud.format.size().height;
                if width == 0 || height == 0 {
                    return;
                }
                let chunk = datas[0].chunk();
                let stride = chunk.stride() as u32;
                let size = chunk.size() as usize;
                let Some(slice) = datas[0].data() else {
                    // No CPU-mapped slice: likely DMA-BUF despite MemPtr/MemFd (#285). Log once.
                    static WARNED: AtomicBool = AtomicBool::new(false);
                    if !WARNED.swap(true, Ordering::Relaxed) {
                        eprintln!(
                            "[capture/pw] no CPU-mapped data on dequeued buffer (type={:?}) — \
                             compositor likely delivered DMA-BUF; no frames will flow",
                            datas[0].type_()
                        );
                    }
                    return;
                };
                if slice.len() < size {
                    return;
                }
                // Copy out before the buffer returns to rotation; last-frame-wins.
                let bgrx = slice[..size].to_vec();
                let _ = tx_for_proc.try_send(CaptureMsg::Frame {
                    width,
                    height,
                    stride,
                    timestamp_us: now_us(),
                    bgrx,
                });
            })
            .register()?;

        // Negotiate BGRx (libwebrtc's argb_to_i420 reads the same byte
        // order on little-endian).
        let obj = pw::spa::pod::object!(
            pw::spa::utils::SpaTypes::ObjectParamFormat,
            pw::spa::param::ParamType::EnumFormat,
            pw::spa::pod::property!(
                pw::spa::param::format::FormatProperties::MediaType,
                Id,
                pw::spa::param::format::MediaType::Video
            ),
            pw::spa::pod::property!(
                pw::spa::param::format::FormatProperties::MediaSubtype,
                Id,
                pw::spa::param::format::MediaSubtype::Raw
            ),
            // Wide format set: compositors differ (KWin BGRx/BGRA, Mutter RGBx/RGBA).
            pw::spa::pod::property!(
                pw::spa::param::format::FormatProperties::VideoFormat,
                Choice,
                Enum,
                Id,
                pw::spa::param::video::VideoFormat::BGRx,
                pw::spa::param::video::VideoFormat::BGRx,
                pw::spa::param::video::VideoFormat::BGRA,
                pw::spa::param::video::VideoFormat::RGBx,
                pw::spa::param::video::VideoFormat::RGBA,
                pw::spa::param::video::VideoFormat::RGB,
                pw::spa::param::video::VideoFormat::BGR,
                pw::spa::param::video::VideoFormat::xRGB,
                pw::spa::param::video::VideoFormat::xBGR,
                pw::spa::param::video::VideoFormat::ARGB,
                pw::spa::param::video::VideoFormat::ABGR,
                pw::spa::param::video::VideoFormat::YUY2,
                pw::spa::param::video::VideoFormat::I420,
                pw::spa::param::video::VideoFormat::NV12
            ),
            // Prefer 1080p; most compositors offer only native size, which the parent accepts.
            pw::spa::pod::property!(
                pw::spa::param::format::FormatProperties::VideoSize,
                Choice,
                Range,
                Rectangle,
                pw::spa::utils::Rectangle { width: 1920, height: 1080 },
                pw::spa::utils::Rectangle { width: 1, height: 1 },
                pw::spa::utils::Rectangle { width: 7680, height: 4320 }
            ),
            // Prefer 60fps (MAX_SHARE_FPS); the parent drops anything faster.
            pw::spa::pod::property!(
                pw::spa::param::format::FormatProperties::VideoFramerate,
                Choice,
                Range,
                Fraction,
                pw::spa::utils::Fraction { num: 60, denom: 1 },
                pw::spa::utils::Fraction { num: 0, denom: 1 },
                pw::spa::utils::Fraction { num: 1000, denom: 1 }
            ),
        );
        let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &pw::spa::pod::Value::Object(obj),
        )?
        .0
        .into_inner();

        // CPU-mappable buffers only: DMA-BUF arrives with `data() == None` and the share goes black.
        let data_type_mask: i32 =
            (1 << pw::spa::sys::SPA_DATA_MemPtr) | (1 << pw::spa::sys::SPA_DATA_MemFd);
        let buffers_obj = pw::spa::pod::Object {
            type_: pw::spa::utils::SpaTypes::ObjectParamBuffers.as_raw(),
            id: pw::spa::param::ParamType::Buffers.as_raw(),
            properties: vec![pw::spa::pod::Property {
                key: pw::spa::sys::SPA_PARAM_BUFFERS_dataType,
                flags: pw::spa::pod::PropertyFlags::empty(),
                value: pw::spa::pod::Value::Choice(pw::spa::pod::ChoiceValue::Int(
                    pw::spa::utils::Choice::<i32>(
                        pw::spa::utils::ChoiceFlags::empty(),
                        pw::spa::utils::ChoiceEnum::<i32>::Flags {
                            default: data_type_mask,
                            flags: vec![data_type_mask],
                        },
                    ),
                )),
            }],
        };
        let buffers_bytes: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &pw::spa::pod::Value::Object(buffers_obj),
        )?
        .0
        .into_inner();

        let format_pod = pw::spa::pod::Pod::from_bytes(&values)
            .ok_or_else(|| anyhow::anyhow!("malformed format pod"))?;
        let buffers_pod = pw::spa::pod::Pod::from_bytes(&buffers_bytes)
            .ok_or_else(|| anyhow::anyhow!("malformed buffers pod"))?;
        let mut params = [format_pod, buffers_pod];

        stream.connect(
            spa::utils::Direction::Input,
            Some(node_id),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )?;
        eprintln!("[capture/pw] video stream connected");

        eprintln!("[capture/pw] entering mainloop");
        mainloop.run();
        eprintln!("[capture/pw] mainloop exited");
        Ok(())
    }

    // Audio lives in `crate::audio` (#884) so the X11 backend gets it too.
}
