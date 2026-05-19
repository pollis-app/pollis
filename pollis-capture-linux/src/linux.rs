use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

const MSG_FORMAT: u8 = 0x01;
const MSG_FRAME: u8 = 0x02;
const MSG_ERROR: u8 = 0xFF;

#[derive(Parser, Debug)]
#[command(name = "pollis-capture-linux", version)]
struct Args {
    /// Path of the Unix socket that the parent (Pollis main) is
    /// listening on. We connect to it once at startup; closing this
    /// socket is the parent's signal to make us exit.
    #[arg(long)]
    socket: String,
}

pub fn run() -> Result<()> {
    let args = Args::parse();

    // PR_SET_PDEATHSIG: if the parent process dies (crash, kill -9,
    // anything other than a clean exit that closes the socket), the
    // kernel sends us SIGTERM so we don't end up an orphan with a live
    // pipewire stream.
    unsafe {
        libc_pdeathsig(libc::SIGTERM);
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move { run_async(&args.socket).await })
}

async fn run_async(socket_path: &str) -> Result<()> {
    eprintln!("[capture] connecting to parent socket {socket_path}");
    let mut sock = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connect to {socket_path}"))?;
    eprintln!("[capture] connected — opening portal (waits for user picker)");

    // Open the portal — shows the user the system source picker.
    let (node_id, fd) = match open_portal().await {
        Ok(v) => {
            eprintln!("[capture] portal returned node_id={}", v.0);
            v
        }
        Err(e) => {
            eprintln!("[capture] portal error: {e}");
            send_error(&mut sock, &format!("portal: {e}")).await.ok();
            return Err(e);
        }
    };

    // Channel from the pipewire OS thread (sync world) into the tokio
    // task (async world) that owns the socket. Capacity 1 with
    // last-frame-wins backpressure: if the socket can't keep up we drop
    // frames rather than block the capture thread.
    let (tx, mut rx) = mpsc::channel::<Msg>(2);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);

    // Video only — see issue #175 for the audio plan. The portal's
    // accept_audio option (per-window, no loopback) needs raw zbus
    // calls because ashpd doesn't expose it. Capturing the system
    // sink monitor as a fallback was tried and produces a feedback
    // loop in any voice room.
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

    // Drain channel -> socket. On any write error (EPIPE, parent gone)
    // flip the stop flag so the pipewire thread exits and we return.
    let result: Result<()> = async {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = write_msg(&mut sock, msg).await {
                eprintln!("[capture] socket write error: {e} — exiting");
                break;
            }
        }
        Ok(())
    }
    .await;

    stop.store(true, Ordering::Relaxed);
    // Don't block — pipewire thread will exit on next iteration; if it
    // hangs the kernel reaps it on process exit.
    drop(pw_thread);
    result
}

enum Msg {
    Format { width: u32, height: u32 },
    Frame {
        width: u32,
        height: u32,
        stride: u32,
        timestamp_us: i64,
        bgrx: Vec<u8>,
    },
}

async fn write_msg(sock: &mut UnixStream, msg: Msg) -> std::io::Result<()> {
    match msg {
        Msg::Format { width, height } => {
            let mut buf = Vec::with_capacity(1 + 4 + 8);
            buf.push(MSG_FORMAT);
            buf.extend_from_slice(&8u32.to_le_bytes());
            buf.extend_from_slice(&width.to_le_bytes());
            buf.extend_from_slice(&height.to_le_bytes());
            sock.write_all(&buf).await
        }
        Msg::Frame { width, height, stride, timestamp_us, bgrx } => {
            let payload_len = (4 + 4 + 4 + 8 + bgrx.len()) as u32;
            let mut header = Vec::with_capacity(1 + 4 + 4 + 4 + 4 + 8);
            header.push(MSG_FRAME);
            header.extend_from_slice(&payload_len.to_le_bytes());
            header.extend_from_slice(&width.to_le_bytes());
            header.extend_from_slice(&height.to_le_bytes());
            header.extend_from_slice(&stride.to_le_bytes());
            header.extend_from_slice(&timestamp_us.to_le_bytes());
            sock.write_all(&header).await?;
            sock.write_all(&bgrx).await
        }
    }
}

async fn send_error(sock: &mut UnixStream, msg: &str) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(1 + 4 + msg.len());
    buf.push(MSG_ERROR);
    buf.extend_from_slice(&(msg.len() as u32).to_le_bytes());
    buf.extend_from_slice(msg.as_bytes());
    sock.write_all(&buf).await
}

async fn open_portal() -> Result<(u32, OwnedFd)> {
    use ashpd::desktop::{
        screencast::{CursorMode, Screencast, SourceType},
        PersistMode,
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
    let response = proxy
        .start(&session, &ashpd::WindowIdentifier::default())
        .await
        .map_err(|e| anyhow!("portal start: {e}"))?
        .response()
        .map_err(|e| anyhow!("portal response: {e}"))?;
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

mod libc {
    pub const SIGTERM: i32 = 15;
    pub const PR_SET_PDEATHSIG: i32 = 1;
    extern "C" {
        pub fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
    }
}

unsafe fn libc_pdeathsig(sig: i32) {
    let _ = libc::prctl(libc::PR_SET_PDEATHSIG, sig as u64, 0, 0, 0);
}

mod pw {
    use super::Msg;
    use anyhow::Result;
    use std::os::fd::OwnedFd;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::sync::mpsc;

    pub fn run_pipewire(
        node_id: u32,
        fd: OwnedFd,
        tx: mpsc::Sender<Msg>,
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
                    let _ = tx_for_format.try_send(Msg::Format { width: w, height: h });
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
                let Some(slice) = datas[0].data() else { return; };
                if slice.len() < size {
                    return;
                }
                // Copy out — the pipewire buffer goes back into rotation
                // as soon as we leave this closure. Last-frame-wins:
                // try_send fails fast when full, we just skip.
                let bgrx = slice[..size].to_vec();
                let _ = tx_for_proc.try_send(Msg::Frame {
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
            // preference hands us a ≤1080p stream (cheapest cap — no
            // per-frame scale needed downstream). The full range stays
            // wide because most compositors ignore the preference and
            // only offer the source's native size; the parent reader
            // (screenshare.rs `convert_and_cap`) then enforces the hard
            // 1920x1080 cap with a libyuv I420 downscale. Doing the cap
            // there rather than re-negotiating here keeps a single
            // last-frame-wins backpressure point and avoids
            // negotiation-failure on compositors that can't resize.
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
        let mut params = [pw::spa::pod::Pod::from_bytes(&values).ok_or_else(|| {
            anyhow::anyhow!("malformed pod")
        })?];

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
