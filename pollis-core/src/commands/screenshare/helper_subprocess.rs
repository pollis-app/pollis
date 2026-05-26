//! Shared helper-subprocess capture path (Linux + macOS).
//!
//! One implementation drives both per-platform helpers. The only
//! per-OS difference is which helper binary is spawned
//! (`capture_helper_name()`); everything after — socket accept, the
//! `pollis-capture-proto` Format/Frame/Error decode, LiveKit publish,
//! FPS cap, libyuv ARGB->I420 — is identical. This is exactly the
//! de-risking #283 Phase 2 buys: every SCK call now runs in a process
//! whose death the parent already tolerates.
//!
//! ── Helper subprocess wire protocol ──────────────────────────────────────
//!
//! The Format/Frame/Error framing now lives in the single shared
//! `pollis-capture-proto` crate (decode: `pollis_capture_proto::read_msg`,
//! used by the start path + reader task in `start_unix`). Both the
//! `pollis-capture-linux` and `pollis-capture-macos` helpers encode with
//! the same crate, so the wire bytes have exactly one definition. The
//! hand-rolled `SocketReader` / `HelperMsg` / `MSG_*` that used to live
//! here were removed in the issue #281/#283 helper-split refactor — no
//! behavior change, the byte layout is identical.

use std::sync::Arc;

use crate::{error::Result, state::AppState};

use super::{fail_capture, state::HelperSession};

/// Spawn the per-platform capture helper and wait for it to connect back
/// over a fresh Unix socket. Returns the established session split into
/// read/write halves so the parent can both send `Select` (macOS picker
/// reply) and read `Format`/`Frame` messages. Used by both
/// `enumerate_screen_sources` (macOS picker phase) and
/// `start_screen_share` (Linux/Windows direct path).
pub(super) async fn spawn_and_accept_helper(
    state: &Arc<AppState>,
) -> Result<HelperSession> {
    use tokio::net::UnixListener;

    let socket_path = std::env::temp_dir().join(format!(
        "pollis-capture-{}-{}.sock",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    let _ = std::fs::remove_file(&socket_path);
    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[screenshare] bind unix socket: {e}");
            return Err(fail_capture(
                state,
                "Could not set up the screen-capture channel. Please try again.".into(),
            )
            .await);
        }
    };

    let helper_path = match locate_capture_helper() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[screenshare] locate helper: {e}");
            return Err(fail_capture(
                state,
                "Screen-capture helper not found. Reinstall Pollis or rebuild the capture helper.".into(),
            )
            .await);
        }
    };
    eprintln!(
        "[screenshare] spawning helper {} on socket {}",
        helper_path.display(),
        socket_path.display()
    );
    let helper = tokio::process::Command::new(&helper_path)
        .arg("--socket")
        .arg(&socket_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn();
    let mut helper = match helper {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[screenshare] spawn {}: {e}", helper_path.display());
            return Err(fail_capture(
                state,
                "Could not launch the screen-capture helper. Please try again.".into(),
            )
            .await);
        }
    };

    let accept_fut = listener.accept();
    let (stream, _addr) = tokio::select! {
        res = accept_fut => match res {
            Ok(r) => {
                eprintln!("[screenshare] helper connected");
                r
            }
            Err(e) => {
                eprintln!("[screenshare] accept: {e}");
                let _ = std::fs::remove_file(&socket_path);
                return Err(fail_capture(
                    state,
                    "Screen-capture helper failed to connect. Please try again.".into(),
                )
                .await);
            }
        },
        status = helper.wait() => {
            eprintln!("[screenshare] helper exited before connecting: {status:?}");
            let _ = std::fs::remove_file(&socket_path);
            return Err(fail_capture(
                state,
                "Screen capture could not start (helper exited). Check screen-capture permission and try again.".into(),
            )
            .await);
        }
    };
    let _ = std::fs::remove_file(&socket_path);

    let (read_half, write_half) = stream.into_split();
    Ok(HelperSession {
        child: helper,
        reader: tokio::io::BufReader::with_capacity(64 * 1024, read_half),
        writer: write_half,
    })
}

/// Resolve the per-platform capture helper binary. Linux ->
/// `pollis-capture-linux`, macOS -> `pollis-capture-macos`. Both ship as
/// Tauri `externalBin` sidecars next to the main binary in production.
fn capture_helper_name() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "pollis-capture-linux"
    }
    #[cfg(target_os = "macos")]
    {
        "pollis-capture-macos"
    }
}

fn locate_capture_helper() -> Result<std::path::PathBuf> {
    use std::path::PathBuf;

    let helper = capture_helper_name();

    // 1. Explicit override — useful for dev setups with a non-standard
    //    layout.
    if let Ok(p) = std::env::var("POLLIS_CAPTURE_BIN") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Ok(p);
        }
    }

    // 2. Sidecar next to the current executable (this is how we ship in
    //    production — Tauri bundles the helper as an external bin).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(helper);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    // 3. Dev: workspace target dir. We can't be sure of profile, so try
    //    the running binary's profile first (debug if the parent is
    //    debug, otherwise release), then fall back to the other.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
    let workspace_root = manifest_dir
        .as_ref()
        .map(PathBuf::from)
        .and_then(|p| p.parent().map(|p| p.to_path_buf()));
    let profiles: &[&str] = if cfg!(debug_assertions) {
        &["debug", "release"]
    } else {
        &["release", "debug"]
    };
    if let Some(root) = workspace_root.as_ref() {
        for profile in profiles {
            let candidate = root.join("target").join(profile).join(helper);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    // Also try a `target/<profile>` relative to CWD — covers
    // `pnpm dev` running from the repo root.
    if let Ok(cwd) = std::env::current_dir() {
        for profile in profiles {
            let candidate = cwd.join("target").join(profile).join(helper);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    Err(crate::error::Error::Other(anyhow::anyhow!(
        "{helper} helper binary not found; set POLLIS_CAPTURE_BIN or build it with `cargo build -p {helper}`"
    )))
}
