//! X11 screen-capture backend (#281), for X11 sessions whose DE ships no
//! ScreenCast portal. Never used under Wayland, where XWayland's private
//! root captures black. See `.codesight/wiki/capture-split.md`.
//!
//! xcb + MIT-SHM (plain GetImage is too slow), one RandR monitor rather than
//! the spanned root, full-frame copy per tick. No picker: X11 has no consent
//! model. TODO (follow-up phases): XDamage, cursor compositing via XFixes,
//! HiDPI / multi-monitor edge geometry.
//!
//! A 24/32-bit ZPixmap on a little-endian server is BGRX, the protocol's
//! format; anything else is rejected rather than shipped miscolored.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use pollis_capture_proto::{now_us, CaptureMsg};
use tokio::sync::mpsc;
use xcb::{shm, x, Xid};

/// Root-window coordinates.
struct CaptureRegion {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

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

fn primary_region(conn: &xcb::Connection, root: x::Window) -> Option<CaptureRegion> {
    let cookie = conn.send_request(&xcb::randr::GetOutputPrimary { window: root });
    let output = conn.wait_for_reply(cookie).ok()?.output();
    // `Xid::is_none()` is X11's `None` (0); xcb 1.x has no free `x::NONE`.
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

/// RandR primary output, else the first active CRTC, else the whole root.
fn pick_region(conn: &xcb::Connection, root: x::Window) -> Result<CaptureRegion> {
    if let Some(region) = primary_region(conn, root).or_else(|| first_active_region(conn, root)) {
        return Ok(region);
    }

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

/// The synchronous SHM loop: one Format, then a Frame per ~60 Hz tick, until
/// `stop` or the channel closes.
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

    let bytes_per_pixel = 4usize;
    let frame_bytes = width as usize * height as usize * bytes_per_pixel;

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
    let shmseg: shm::Seg = conn.generate_id();
    conn.send_and_check_request(&shm::Attach {
        shmseg,
        shmid: shmid as u32,
        read_only: false,
    })
    .context("shm::Attach (MIT-SHM not available?)")?;
    // Remove-on-detach now, so the segment can't leak even if we crash.
    unsafe {
        libc::shmctl(shmid, libc::IPC_RMID, std::ptr::null_mut());
    }

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

    if tx
        .blocking_send(CaptureMsg::Format {
            width: width as u32,
            height: height as u32,
        })
        .is_err()
    {
        return Ok(());
    }

    // Producer-side spacer so full-frame copies don't peg a core; the parent
    // also clamps to MAX_SHARE_FPS.
    let frame_interval = Duration::from_nanos(1_000_000_000 / 60);
    let slice =
        unsafe { std::slice::from_raw_parts(shmaddr as *const u8, frame_bytes) };

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let tick_start = Instant::now();

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

        // The 32-bit scanline pad is one pixel, so the stride is width * 4.
        let stride = width as u32 * bytes_per_pixel as u32;
        let timestamp_us = now_us();

        // Copy out: the next GetImage overwrites the segment.
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
            // Full drops the frame; closed means the parent is gone.
            if tx.is_closed() {
                break;
            }
        }

        let elapsed = tick_start.elapsed();
        if elapsed < frame_interval {
            std::thread::sleep(frame_interval - elapsed);
        }
    }

    // Detach the X side before the guard detaches our mapping.
    let _ = conn.send_and_check_request(&shm::Detach { shmseg });
    Ok(())
}
