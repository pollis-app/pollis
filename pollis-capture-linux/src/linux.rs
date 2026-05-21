use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use pollis_capture_proto::{read_msg, write_msg, CaptureMsg, Selection, SourceList};
use tokio::io::BufReader;
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
///
/// Order: portal first (regardless of session type), X11 fallback. The
/// portal gives users the proper system picker with per-window selection
/// on GNOME-on-X11 / KDE-on-X11 too, not just Wayland — so we no longer
/// gate it on session type. X11 backend remains the fallback for
/// Cinnamon/MATE/XFCE (no ScreenCast portal backend exists) and other
/// X11 sessions where the portal is absent. We still refuse to pick
/// X11 under Wayland because XWayland hands an X11 client a private
/// root window, so xcb/SHM would capture a black surface.
async fn probe_backend() -> Backend {
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

    if portal_screencast_available().await {
        eprintln!("[capture] probe: ScreenCast portal available -> Portal backend");
        return Backend::Portal;
    }

    if is_x11 && !is_wayland {
        eprintln!("[capture] probe: no portal, X11 session -> X11 backend");
        return Backend::X11;
    }

    eprintln!(
        "[capture] probe: no portal{} -> Unsupported",
        if is_wayland { ", Wayland session" } else { "" }
    );
    Backend::Unsupported
}

/// Is `org.freedesktop.portal.ScreenCast` actually available on the
/// session bus? A bare `xdg-desktop-portal` with only the gtk backend
/// (Cinnamon/MATE/XFCE default) does NOT implement ScreenCast — the
/// interface is absent and the portal call errors before any picker UI.
/// We detect that here so we can emit a precise "your DE has no
/// ScreenCast backend" error instead of collapsing it into "denied".
async fn portal_screencast_available() -> bool {
    use ashpd::desktop::screencast::Screencast;
    // Constructing the proxy resolves the org.freedesktop.portal.Desktop
    // name and the ScreenCast interface. If the backend doesn't
    // implement ScreenCast this fails fast without showing any UI.
    match Screencast::new().await {
        Ok(proxy) => {
            // available_source_types() is a plain property read on the
            // ScreenCast interface — succeeds only if a real backend is
            // wired up, which is exactly the distinction we need.
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

    // PR_SET_PDEATHSIG: if the parent process dies (crash, kill -9,
    // anything other than a clean exit that closes the socket), the
    // kernel sends us SIGTERM so we don't end up an orphan with a live
    // pipewire stream.
    unsafe {
        libc_pdeathsig(pdeath::SIGTERM);
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move { run_async(&args.socket).await })
}

async fn run_async(socket_path: &str) -> Result<()> {
    eprintln!("[capture] connecting to parent socket {socket_path}");
    let sock = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connect to {socket_path}"))?;
    eprintln!("[capture] connected — probing session backend");

    let backend = probe_backend().await;

    // Unsupported: tell the parent and exit. No Sources round-trip — the
    // parent's `enumerate_screen_sources` reads our Error and surfaces
    // "your desktop environment doesn't support screen sharing".
    if backend == Backend::Unsupported {
        let msg = "unsupported: Screen sharing requires xdg-desktop-portal \
                   with a ScreenCast backend; your desktop environment does \
                   not provide one.";
        eprintln!("[capture] {msg}");
        let (_r, mut w) = sock.into_split();
        write_msg(&mut w, &CaptureMsg::Error { message: msg.into() })
            .await
            .ok();
        return Err(anyhow!("no ScreenCast portal backend"));
    }

    // Enumerate sources up-front so the parent can render an in-app
    // picker on X11 (where there's no system picker) and immediately
    // return an empty list on Portal (where the portal owns its own
    // picker). This matches the macOS protocol: Sources → optional
    // Select → Format → Frames.
    let sources = match backend {
        Backend::Portal => SourceList { displays: Vec::new(), windows: Vec::new() },
        Backend::X11 => SourceList {
            displays: crate::x11::enumerate_outputs().unwrap_or_else(|e| {
                eprintln!("[capture/x11] enumerate failed, falling back to default: {e}");
                Vec::new()
            }),
            windows: Vec::new(),
        },
        Backend::Unsupported => unreachable!("handled above"),
    };
    let needs_select = !sources.displays.is_empty() || !sources.windows.is_empty();

    let (read_half, write_half) = sock.into_split();
    let mut reader = BufReader::with_capacity(8 * 1024, read_half);
    let mut writer = write_half;

    eprintln!(
        "[capture] sending Sources ({} displays, {} windows)",
        sources.displays.len(),
        sources.windows.len()
    );
    if let Err(e) = write_msg(&mut writer, &CaptureMsg::Sources(sources)).await {
        return Err(anyhow!("write Sources: {e}"));
    }

    // Wait for the user's pick from the in-app picker, but only if we
    // actually offered choices. Empty Sources means "no picker shown,
    // proceed with default capture" — for Portal the OS picker takes
    // over from here; for X11 we fall back to `pick_region(None)`.
    let selected_output_id: Option<u32> = if needs_select {
        eprintln!("[capture] awaiting Select from parent");
        match read_msg(&mut reader).await {
            Ok(Some(CaptureMsg::Select(Selection::Display { id }))) => {
                eprintln!("[capture] parent selected display id={id}");
                Some(id)
            }
            Ok(Some(CaptureMsg::Select(Selection::Window { id: _ }))) => {
                // No X11 window capture in v1. Treat as "use default
                // monitor" rather than fail — the user can re-pick.
                eprintln!("[capture] window selection unsupported on X11; using default monitor");
                None
            }
            Ok(Some(other)) => {
                return Err(anyhow!("expected Select, got {other:?}"));
            }
            Ok(None) => {
                // Parent closed the socket — user cancelled before
                // picking. Quiet exit; the picker session cleanup on
                // the parent side already handles UI state.
                eprintln!("[capture] parent closed socket before Select; exiting");
                return Ok(());
            }
            Err(e) => return Err(anyhow!("read Select: {e}")),
        }
    } else {
        None
    };

    match backend {
        Backend::Portal => run_portal(&mut writer).await,
        Backend::X11 => {
            // The X11 backend is synchronous (xcb + SHM) and has no
            // async work; run it on a blocking thread and bridge frames
            // back over the same channel-to-socket drain.
            run_x11(&mut writer, selected_output_id).await
        }
        Backend::Unsupported => unreachable!("handled above"),
    }
}

/// The original Wayland + xdg-desktop-portal + PipeWire path. Logic
/// unchanged from before the split — only the wire encode now goes
/// through the shared `pollis-capture-proto` crate instead of the
/// hand-rolled `write_msg`/`send_error` that used to live here.
async fn run_portal(sock: &mut tokio::net::unix::OwnedWriteHalf) -> Result<()> {
    eprintln!("[capture] opening portal (waits for user picker)");

    let (node_id, fd) = match open_portal().await {
        Ok(v) => {
            eprintln!("[capture] portal returned node_id={}", v.0);
            v
        }
        Err(PortalError::Cancelled) => {
            // User dismissed the picker. Surface with a distinct
            // `cancel:` prefix so the parent can swallow the error
            // silently instead of showing a red "permission denied"
            // toast — cancelling is a normal flow, not a failure.
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
            // Prefix lets the parent split portal-error vs. cancel vs.
            // genuine deny rather than collapsing them.
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

    // Channel from the pipewire OS thread (sync world) into the tokio
    // task (async world) that owns the socket. Capacity 2 with
    // last-frame-wins backpressure: if the socket can't keep up we drop
    // frames rather than block the capture thread.
    let (tx, mut rx) = mpsc::channel::<CaptureMsg>(2);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);

    eprintln!("[capture] spawning pipewire video thread");
    let pw_thread = std::thread::Builder::new()
        .name("pollis-capture-pw".into())
        .spawn(move || {
            eprintln!("[capture/pw] thread entered");
            if let Err(e) = pw::run_pipewire(node_id, fd, tx, stop_for_thread) {
                eprintln!("[capture/pw] error: {e}");
            }
            eprintln!("[capture/pw] thread exiting");
        })
        .context("spawn pipewire thread")?;

    let result: Result<()> = async {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = write_msg(sock, &msg).await {
                eprintln!("[capture] socket write error: {e} — exiting");
                break;
            }
        }
        Ok(())
    }
    .await;

    stop.store(true, Ordering::Relaxed);
    drop(pw_thread);
    result
}

/// The X11 backend (issue #281). Spawns the synchronous xcb/SHM capture
/// loop on its own thread and drains its frames to the socket exactly
/// the way the portal path drains the pipewire thread.
async fn run_x11(
    sock: &mut tokio::net::unix::OwnedWriteHalf,
    selected_output_id: Option<u32>,
) -> Result<()> {
    eprintln!(
        "[capture] starting X11 (xcb/SHM/RandR) backend (selection={:?})",
        selected_output_id
    );

    let (tx, mut rx) = mpsc::channel::<CaptureMsg>(2);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);

    let x11_thread = std::thread::Builder::new()
        .name("pollis-capture-x11".into())
        .spawn(move || {
            eprintln!("[capture/x11] thread entered");
            if let Err(e) = crate::x11::run_x11_capture(
                tx.clone(),
                stop_for_thread,
                selected_output_id,
            ) {
                eprintln!("[capture/x11] error: {e}");
                let _ = tx.blocking_send(CaptureMsg::Error {
                    message: format!("x11: {e}"),
                });
            }
            eprintln!("[capture/x11] thread exiting");
        })
        .context("spawn x11 thread")?;

    let result: Result<()> = async {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = write_msg(sock, &msg).await {
                eprintln!("[capture] socket write error: {e} — exiting");
                break;
            }
        }
        Ok(())
    }
    .await;

    stop.store(true, Ordering::Relaxed);
    drop(x11_thread);
    result
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
        // The picker callback returned a Cancelled response — user hit
        // Escape, closed the window, or clicked Cancel. Surface this
        // specifically; do NOT wrap in anyhow! since the parent's
        // friendly-error mapping must see the structured signal.
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
    // Leak the proxy + session — dropping them closes the screencast
    // session on the portal side, which silently kills the pipewire
    // stream (the fd stays open but produces no frames). The helper
    // process's lifetime is the share's lifetime, so the leak is
    // bounded.
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
    use pollis_capture_proto::CaptureMsg;
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
        // Same fd serves both video (from the portal pipewire node) and
        // the audio sink monitor capture. The portal-issued fd is a
        // fully-fledged pipewire core fd, not a per-stream descriptor.
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
                    // No CPU-mapped slice — almost certainly because the
                    // compositor handed us a DMA-BUF buffer despite our
                    // dataType param constraining to MemPtr|MemFd. Log
                    // once so the next time something like #285's
                    // "whole-monitor black preview" recurs, we see the
                    // smoking gun instead of a silent drop.
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
                // Copy out — the pipewire buffer goes back into rotation
                // as soon as we leave this closure. Last-frame-wins:
                // try_send fails fast when full, we just skip.
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
            // Wide format set — different compositors prefer different
            // pixel layouts (KWin tends to ship BGRx/BGRA, Mutter often
            // RGBx/RGBA, some DMA-BUF backends only YUV). Listing all
            // common variants lets pipewire pick whichever it has
            // available without us having to fingerprint the compositor.
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
            // First Rectangle = preferred/default. We bias the negotiated
            // size toward 1080p so a compositor that honours the
            // preference hands us a sensible default stream size. The
            // full range stays wide because most compositors ignore the
            // preference and only offer the source's native size; the
            // parent reader (screenshare.rs) accepts whatever lands
            // here at native resolution — no downstream cap is applied.
            pw::spa::pod::property!(
                pw::spa::param::format::FormatProperties::VideoSize,
                Choice,
                Range,
                Rectangle,
                pw::spa::utils::Rectangle { width: 1920, height: 1080 },
                pw::spa::utils::Rectangle { width: 1, height: 1 },
                pw::spa::utils::Rectangle { width: 7680, height: 4320 }
            ),
            // Preferred 60fps (matches the parent's MAX_SHARE_FPS cap);
            // the parent reader hard-drops anything faster. Range stays
            // open so a compositor that only offers its native refresh
            // still negotiates — the parent FPS clamp is the backstop.
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

        // Buffers param: constrain the producer to CPU-mappable buffer
        // types (MemPtr / MemFd), excluding DMA-BUF. Without this,
        // KWin/Mutter happily hand back DMA-BUF for whole-monitor
        // capture (zero-copy GPU texture). MAP_BUFFERS only maps
        // CPU-mappable backings — DMA-BUF would arrive with
        // `data() == None`, every frame would silently drop, and the
        // whole-monitor share would look black on the publisher and
        // never produce a track-started event on the receiver.
        // Reading DMA-BUF would require importing it into an EGL/Vulkan
        // context; we don't have one, and don't want one in this
        // helper.
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

    fn now_us() -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0)
    }

    // Audio thread removed — see issue #175. The system sink monitor
    // capture worked end-to-end but loops voice back through the
    // call. Per-window audio needs the portal's `accept_audio` option
    // (no loopback by construction) which requires raw zbus calls
    // because ashpd doesn't expose it. Re-add when that lands.
}
