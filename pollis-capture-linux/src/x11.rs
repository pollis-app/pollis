//! X11 screen-capture backend (issue #281).
//!
//! Why this exists: on an X11 session there is frequently no
//! xdg-desktop-portal ScreenCast backend at all — Cinnamon/MATE/XFCE
//! ship only `xdg-desktop-portal-gtk`, which does NOT implement
//! ScreenCast. The portal call errors before any picker appears, which
//! the old code collapsed into "user denied". On X11 we don't need the
//! portal: we can read the framebuffer directly.
//!
//! Why not X11 everywhere: under Wayland, XWayland gives an X11 client a
//! *private* root window, not the real composited screen. XShm/XGetImage
//! against it returns black. So this backend is only ever selected for a
//! genuine X11 session (see `Backend::X11` in `linux.rs`).
//!
//! v1 scope (shippable, deliberately minimal):
//!   - xcb + MIT-SHM. SHM is non-negotiable: a plain GetImage round-trip
//!     at 1080p is unusably slow (full pixmap over the X socket every
//!     frame).
//!   - RandR is used to enumerate outputs and capture ONE monitor
//!     (the primary, or the first active output), not the whole spanned
//!     root — a multi-monitor spanned root would publish a giant canvas.
//!   - Full-framebuffer SHM copy per tick. Correct; heavier on weak
//!     CPUs. XDamage (changed-region only) is Phase 2, intentionally out
//!     of v1 (see issue #281 follow-ups).
//!   - No per-window consent picker: X11 has no consent model, so this
//!     is monitor / full-screen capture only. Source selection therefore
//!     reuses the protocol shape (a single Format then frames) without a
//!     picker round-trip.
//!
//! Out of v1 (documented TODOs, NOT blockers):
//!   - Phase 2: XDamage — only copy changed regions.
//!   - Phase 3: cursor compositing via XFixes GetCursorImage.
//!   - Phase 4: HiDPI / fractional scaling; multi-monitor edge geometry.
//!
//! Output pixel format: we request a `ZPixmap` from a 24/32-bit
//! TrueColor visual. On the overwhelmingly common little-endian X
//! server with a 32-bpp visual the byte order is B,G,R,X — exactly the
//! BGRx the shared protocol and the parent's `argb_to_i420` expect. We
//! validate the depth/bpp and bail with a clear error rather than ship
//! wrong colors if a server hands us something exotic.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use pollis_capture_proto::{CaptureMsg, DisplaySource};
use tokio::sync::mpsc;
use xcb::{shm, x, Xid, XidNew};

/// Geometry of the monitor we capture, in root-window coordinates.
struct CaptureRegion {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

/// Enumerate the RandR outputs as `DisplaySource`s for the in-app
/// picker. The `id` is the RandR output XID — a u32 stable for the
/// lifetime of the X server, so passing it back in a later `Selection`
/// from the parent still resolves to the same physical output.
///
/// Connected outputs without an active CRTC (unplugged-but-listed) are
/// filtered out — capturing them returns black. RandR-absent servers
/// return an empty list; the parent treats empty as "no picker, fall
/// back to the default capture region" via `pick_region(None)`.
pub fn enumerate_outputs() -> Result<Vec<DisplaySource>> {
    let (conn, screen_num) = xcb::Connection::connect_with_extensions(
        None,
        &[xcb::Extension::RandR],
        &[],
    )
    .context("xcb connect for enumeration")?;
    let setup = conn.get_setup();
    let screen = setup
        .roots()
        .nth(screen_num as usize)
        .ok_or_else(|| anyhow!("no X screen {screen_num}"))?;
    let root = screen.root();

    let res_cookie = conn.send_request(&xcb::randr::GetScreenResourcesCurrent { window: root });
    let res = match conn.wait_for_reply(res_cookie) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[capture/x11] RandR unavailable, no outputs to enumerate: {e}");
            return Ok(Vec::new());
        }
    };

    let primary = conn
        .send_request(&xcb::randr::GetOutputPrimary { window: root });
    let primary_id: u32 = conn
        .wait_for_reply(primary)
        .ok()
        .map(|r| r.output().resource_id())
        .unwrap_or(0);

    let mut displays: Vec<DisplaySource> = Vec::new();
    for &output in res.outputs() {
        if output.is_none() {
            continue;
        }
        let info_cookie = conn.send_request(&xcb::randr::GetOutputInfo {
            output,
            config_timestamp: x::CURRENT_TIME,
        });
        let info = match conn.wait_for_reply(info_cookie) {
            Ok(i) => i,
            Err(_) => continue,
        };
        let crtc = info.crtc();
        if crtc.is_none() {
            continue;
        }
        let crtc_cookie = conn.send_request(&xcb::randr::GetCrtcInfo {
            crtc,
            config_timestamp: x::CURRENT_TIME,
        });
        let crtc_info = match conn.wait_for_reply(crtc_cookie) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if crtc_info.width() == 0 || crtc_info.height() == 0 {
            continue;
        }
        let name = String::from_utf8_lossy(info.name()).into_owned();
        let id = output.resource_id();
        let label = if id == primary_id {
            format!("{} (primary)", name)
        } else {
            name
        };
        displays.push(DisplaySource {
            id,
            width: crtc_info.width() as u32,
            height: crtc_info.height() as u32,
            name: label,
        });
    }

    // Primary first — matches what users expect to see in the picker.
    displays.sort_by_key(|d| if d.id == primary_id { 0 } else { 1 });
    Ok(displays)
}

/// Pick the monitor to capture. If `selected_output_id` is Some, locate
/// that RandR output and capture its CRTC. Otherwise prefer the RandR
/// primary output; fall back to the first connected/active CRTC;
/// finally fall back to the whole root window if RandR is unavailable
/// (very old server).
fn pick_region(
    conn: &xcb::Connection,
    root: x::Window,
    selected_output_id: Option<u32>,
) -> Result<CaptureRegion> {
    if let Some(wanted) = selected_output_id {
        if let Some(region) = region_for_output(conn, root, wanted) {
            return Ok(region);
        }
        eprintln!(
            "[capture/x11] selected output {wanted} not found / inactive; falling back to default"
        );
    }

    // Try RandR primary first.
    let primary = conn.send_request(&xcb::randr::GetOutputPrimary { window: root });
    if let Ok(primary) = conn.wait_for_reply(primary) {
        let output = primary.output();
        // `Xid::is_none()` lives on every XID newtype the xcb 1.x bindings
        // expose (Output / Crtc / Mode / …). It's the typed equivalent of
        // X11's `None` sentinel — `0` for the resource id — without
        // assuming the generated bindings expose a free `x::NONE` const
        // (they don't — only the atom-shaped one, `x::ATOM_NONE`).
        if !output.is_none() {
            let info = conn.send_request(&xcb::randr::GetOutputInfo {
                output,
                config_timestamp: x::CURRENT_TIME,
            });
            if let Ok(info) = conn.wait_for_reply(info) {
                let crtc = info.crtc();
                if !crtc.is_none() {
                    let crtc_info = conn.send_request(&xcb::randr::GetCrtcInfo {
                        crtc,
                        config_timestamp: x::CURRENT_TIME,
                    });
                    if let Ok(c) = conn.wait_for_reply(crtc_info) {
                        if c.width() > 0 && c.height() > 0 {
                            eprintln!(
                                "[capture/x11] capturing primary output {}x{} at +{}+{}",
                                c.width(),
                                c.height(),
                                c.x(),
                                c.y()
                            );
                            return Ok(CaptureRegion {
                                x: c.x(),
                                y: c.y(),
                                width: c.width(),
                                height: c.height(),
                            });
                        }
                    }
                }
            }
        }
    }

    // No primary — first active CRTC.
    let res = conn.send_request(&xcb::randr::GetScreenResourcesCurrent { window: root });
    if let Ok(res) = conn.wait_for_reply(res) {
        for &crtc in res.crtcs() {
            let crtc_info = conn.send_request(&xcb::randr::GetCrtcInfo {
                crtc,
                config_timestamp: x::CURRENT_TIME,
            });
            if let Ok(c) = conn.wait_for_reply(crtc_info) {
                if c.width() > 0 && c.height() > 0 && !c.mode().is_none() {
                    eprintln!(
                        "[capture/x11] capturing CRTC {}x{} at +{}+{}",
                        c.width(),
                        c.height(),
                        c.x(),
                        c.y()
                    );
                    return Ok(CaptureRegion {
                        x: c.x(),
                        y: c.y(),
                        width: c.width(),
                        height: c.height(),
                    });
                }
            }
        }
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

/// Resolve a specific RandR output XID to its CRTC region, or `None` if
/// the output doesn't exist / isn't active. Used by `pick_region` when
/// the parent passed back a user-chosen `Selection::Display`.
fn region_for_output(
    conn: &xcb::Connection,
    _root: x::Window,
    wanted_id: u32,
) -> Option<CaptureRegion> {
    if wanted_id == 0 {
        return None;
    }
    let output = <xcb::randr::Output as XidNew>::new(wanted_id);
    let info_cookie = conn.send_request(&xcb::randr::GetOutputInfo {
        output,
        config_timestamp: x::CURRENT_TIME,
    });
    let info = conn.wait_for_reply(info_cookie).ok()?;
    let crtc = info.crtc();
    if crtc.is_none() {
        return None;
    }
    let crtc_cookie = conn.send_request(&xcb::randr::GetCrtcInfo {
        crtc,
        config_timestamp: x::CURRENT_TIME,
    });
    let c = conn.wait_for_reply(crtc_cookie).ok()?;
    if c.width() == 0 || c.height() == 0 {
        return None;
    }
    eprintln!(
        "[capture/x11] capturing selected output {} {}x{} at +{}+{}",
        wanted_id,
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

/// Run the synchronous SHM capture loop. Sends one Format then a Frame
/// per tick (capped to ~60 Hz; the parent reader also enforces its own
/// MAX_SHARE_FPS clamp, so this is just to avoid a busy spin). Returns
/// when `stop` is set or the channel/socket is gone.
///
/// `selected_output_id` is the RandR output XID the user picked in the
/// in-app picker. `None` means "no picker round-trip happened" — pick
/// the default (primary / first CRTC / root window).
pub fn run_x11_capture(
    tx: mpsc::Sender<CaptureMsg>,
    stop: Arc<AtomicBool>,
    selected_output_id: Option<u32>,
) -> Result<()> {
    // Connect with the RandR + MIT-SHM extensions.
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
    // We rely on a 24/32-bpp TrueColor framebuffer so the ZPixmap byte
    // order is BGRX on a little-endian server. Reject anything else
    // loudly rather than publish miscolored frames.
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

    let region = pick_region(&conn, root, selected_output_id)?;
    let width = region.width;
    let height = region.height;
    if width == 0 || height == 0 {
        return Err(anyhow!("X11 capture region is zero-sized"));
    }

    // 4 bytes per pixel (BGRX). Full-framebuffer buffer, reused every
    // tick — v1 copies the whole region each frame (Phase 2 = XDamage).
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

    // Announce the source size once. The parent creates the LiveKit
    // track from this.
    if tx
        .blocking_send(CaptureMsg::Format {
            width: width as u32,
            height: height as u32,
        })
        .is_err()
    {
        return Ok(());
    }

    // ~60 Hz tick. The parent reader also clamps to MAX_SHARE_FPS; this
    // is the cheap producer-side spacer so we don't peg a core on a weak
    // CPU doing full-frame copies.
    let frame_interval = Duration::from_nanos(1_000_000_000 / 60);
    let slice =
        unsafe { std::slice::from_raw_parts(shmaddr as *const u8, frame_bytes) };

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let tick_start = Instant::now();

        // ZPixmap into the SHM segment. Synchronous round-trip but the
        // pixels travel via shared memory, not the X socket.
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

        // SHM stride for a ZPixmap is width * 4 (no row padding for
        // 32bpp at these widths on the common servers; X pads to the
        // scanline pad which is 32 bits == one pixel, so width*4 holds).
        let stride = width as u32 * bytes_per_pixel as u32;
        let timestamp_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);

        // Copy out of SHM — the next GetImage overwrites it. Last-frame-
        // wins: try_send fails fast when the socket can't keep up.
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
            // Either full (drop this frame, last-frame-wins) or the
            // receiver is gone (parent closed the socket -> exit).
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
