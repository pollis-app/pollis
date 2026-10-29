//! X11 screen-capture backend (issue #281).
//!
//! X11 sessions often lack a ScreenCast portal (Cinnamon/MATE/XFCE), so read the framebuffer directly.
//!
//! Never used under Wayland: XWayland's private root captures black.
//!
//! Output: a 24/32-bit TrueColor ZPixmap, BGRx on little-endian servers; anything else errors.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use pollis_capture_proto::{now_us, CaptureMsg};
use tokio::sync::mpsc;
use xcb::{shm, x, Xid};

/// Geometry of the monitor we capture, in root-window coordinates.
struct CaptureRegion {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

/// Fetch a CRTC's geometry, or `None` if the request fails.
fn crtc_info(
    conn: &xcb::Connection,
    crtc: xcb::randr::Crtc,
) -> Option<xcb::randr::GetCrtcInfoReply> {
    let cookie = conn.send_request(&xcb::randr::GetCrtcInfo {
        crtc,
        config_timestamp: x::CURRENT_TIME,
    });
    conn.wait_for_reply(cookie).ok()
}

impl CaptureRegion {
    /// A CRTC becomes a capture region only if it has a non-zero size.
    fn from_crtc(c: &xcb::randr::GetCrtcInfoReply, what: &str) -> Option<Self> {
        if c.width() == 0 || c.height() == 0 {
            return None;
        }
        eprintln!(
            "[capture/x11] capturing {what} {}x{} at +{}+{}",
            c.width(),
            c.height(),
            c.x(),
            c.y()
        );
        Some(CaptureRegion {
            x: c.x(),
            y: c.y(),
            width: c.width(),
            height: c.height(),
        })
    }
}

/// The RandR primary output's CRTC, if there is one and it is usable.
fn primary_region(conn: &xcb::Connection, root: x::Window) -> Option<CaptureRegion> {
    let cookie = conn.send_request(&xcb::randr::GetOutputPrimary { window: root });
    let output = conn.wait_for_reply(cookie).ok()?.output();
    // Typed equivalent of X11's `None`; xcb has no free `x::NONE` const.
    if output.is_none() {
        return None;
    }
    let cookie = conn.send_request(&xcb::randr::GetOutputInfo {
        output,
        config_timestamp: x::CURRENT_TIME,
    });
    let crtc = conn.wait_for_reply(cookie).ok()?.crtc();
    if crtc.is_none() {
        return None;
    }
    CaptureRegion::from_crtc(&crtc_info(conn, crtc)?, "primary output")
}

/// The first active CRTC RandR reports, if any.
fn first_active_region(conn: &xcb::Connection, root: x::Window) -> Option<CaptureRegion> {
    let cookie = conn.send_request(&xcb::randr::GetScreenResourcesCurrent { window: root });
    let res = conn.wait_for_reply(cookie).ok()?;
    res.crtcs().iter().find_map(|&crtc| {
        let c = crtc_info(conn, crtc)?;
        if c.mode().is_none() {
            return None;
        }
        CaptureRegion::from_crtc(&c, "CRTC")
    })
}

/// Prefer the RandR primary output, then the first active CRTC, then the whole root.
fn pick_region(conn: &xcb::Connection, root: x::Window) -> Result<CaptureRegion> {
    if let Some(region) = primary_region(conn, root).or_else(|| first_active_region(conn, root)) {
        return Ok(region);
    }

    // RandR absent or no usable output: whole root window.
    let geom = conn.send_request(&x::GetGeometry {
        drawable: x::Drawable::Window(root),
    });
    let geom = conn
        .wait_for_reply(geom)
        .context("GetGeometry(root) failed")?;
    eprintln!(
        "[capture/x11] RandR unavailable — capturing whole root {}x{}",
        geom.width(),
        geom.height()
    );
    Ok(CaptureRegion {
        x: 0,
        y: 0,
        width: geom.width(),
        height: geom.height(),
    })
}

/// Synchronous SHM capture loop: one Format, then a Frame per ~60 Hz tick, until `stop`.
pub fn run_x11_capture(tx: mpsc::Sender<CaptureMsg>, stop: Arc<AtomicBool>) -> Result<()> {
    let (conn, screen_num) = xcb::Connection::connect_with_extensions(
        None,
        &[xcb::Extension::Shm, xcb::Extension::RandR],
        &[],
    )
    .context("xcb connect (is $DISPLAY set / X server reachable?)")?;

    let setup = conn.get_setup();
    let screen = setup
        .roots()
        .nth(screen_num as usize)
        .ok_or_else(|| anyhow!("no X screen {screen_num}"))?;
    let root = screen.root();
    let root_depth = screen.root_depth();
    // Reject non-24/32-bpp roots rather than publish miscolored frames.
    if root_depth != 24 && root_depth != 32 {
        return Err(anyhow!(
            "unsupported X root depth {root_depth} (need 24 or 32 for BGRx capture)"
        ));
    }
    if setup.image_byte_order() != x::ImageOrder::LsbFirst {
        return Err(anyhow!(
            "big-endian X server image byte order is unsupported by the v1 X11 backend"
        ));
    }

    let region = pick_region(&conn, root)?;
    let width = region.width;
    let height = region.height;
    if width == 0 || height == 0 {
        return Err(anyhow!("X11 capture region is zero-sized"));
    }

    // 4 bytes per pixel (BGRX), full region copied every tick.
    let bytes_per_pixel = 4usize;
    let frame_bytes = width as usize * height as usize * bytes_per_pixel;

    // MIT-SHM segment sized for one full frame.
    let shmid = unsafe {
        libc::shmget(
            libc::IPC_PRIVATE,
            frame_bytes,
            libc::IPC_CREAT | 0o600,
        )
    };
    if shmid < 0 {
        return Err(anyhow!("shmget({frame_bytes}) failed"));
    }
    let shmaddr = unsafe { libc::shmat(shmid, std::ptr::null(), 0) };
    if shmaddr == (usize::MAX as *mut libc::c_void) || shmaddr.is_null() {
        unsafe {
            libc::shmctl(shmid, libc::IPC_RMID, std::ptr::null_mut());
        }
        return Err(anyhow!("shmat failed"));
    }
    // Mark the segment removed-on-detach now so it can never leak even
    // if we crash: the kernel keeps it alive until the last detach.
    let shmseg: shm::Seg = conn.generate_id();
    conn.send_and_check_request(&shm::Attach {
        shmseg,
        shmid: shmid as u32,
        read_only: false,
    })
    .context("shm::Attach (MIT-SHM not available?)")?;
    unsafe {
        libc::shmctl(shmid, libc::IPC_RMID, std::ptr::null_mut());
    }

    // Cleanup guard for the SHM detach + X seg detach.
    struct ShmGuard {
        addr: *mut libc::c_void,
    }
    impl Drop for ShmGuard {
        fn drop(&mut self) {
            unsafe {
                libc::shmdt(self.addr);
            }
        }
    }
    let _guard = ShmGuard { addr: shmaddr };

    // Announce the source size once; the parent creates the track from it.
    if tx
        .blocking_send(CaptureMsg::Format {
            width: width as u32,
            height: height as u32,
        })
        .is_err()
    {
        return Ok(());
    }

    // ~60 Hz producer-side pacing so full-frame copies don't peg a core.
    let frame_interval = Duration::from_nanos(1_000_000_000 / 60);
    let slice =
        unsafe { std::slice::from_raw_parts(shmaddr as *const u8, frame_bytes) };

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let tick_start = Instant::now();

        // ZPixmap into the SHM segment; pixels skip the X socket.
        let cookie = conn.send_request(&shm::GetImage {
            drawable: x::Drawable::Window(root),
            x: region.x,
            y: region.y,
            width,
            height,
            plane_mask: u32::MAX,
            format: x::ImageFormat::ZPixmap as u8,
            shmseg,
            offset: 0,
        });
        match conn.wait_for_reply(cookie) {
            Ok(_) => {}
            Err(e) => {
                return Err(anyhow!("shm::GetImage failed: {e}"));
            }
        }

        // Stride is width * 4: 32bpp scanlines pad to one pixel.
        let stride = width as u32 * bytes_per_pixel as u32;
        let timestamp_us = now_us();

        // Copy out of SHM before the next GetImage; last-frame-wins.
        let bgrx = slice.to_vec();
        if tx
            .try_send(CaptureMsg::Frame {
                width: width as u32,
                height: height as u32,
                stride,
                timestamp_us,
                bgrx,
            })
            .is_err()
        {
            // Full (drop, last-frame-wins) or receiver gone (exit).
            if tx.is_closed() {
                break;
            }
        }

        // Pace to ~60 Hz.
        let elapsed = tick_start.elapsed();
        if elapsed < frame_interval {
            std::thread::sleep(frame_interval - elapsed);
        }
    }

    // Detach the X side of the SHM seg before the guard detaches our
    // mapping.
    let _ = conn.send_and_check_request(&shm::Detach { shmseg });
    Ok(())
}
