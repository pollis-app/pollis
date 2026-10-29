//! The helper-subprocess capture path (Linux + macOS). Only the helper binary
//! differs per OS; the wire format is `pollis-capture-proto`.

use std::sync::Arc;

use crate::{error::Result, state::AppState};

use super::{fail_capture, state::HelperSession};

/// Spawn the platform capture helper and wait for it to connect back over a
/// fresh Unix socket.
///
/// `with_audio` is a spawn-time flag because the Linux helper has no Select
/// round trip (the portal is its picker). macOS ignores it and reads
/// `Selection::with_audio`, since its helper is spawned before the user sees
/// the audio toggle.
pub(super) async fn spawn_and_accept_helper(
    state: &Arc<AppState>,
    with_audio: bool,
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
            crate::diag!("[screenshare] bind unix socket: {e}");
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
            crate::diag!("[screenshare] locate helper: {e}");
            return Err(fail_capture(
                state,
                "Screen-capture helper not found. Reinstall Pollis or rebuild the capture helper.".into(),
            )
            .await);
        }
    };
    crate::diag!(
        "[screenshare] spawning helper {} on socket {}",
        helper_path.display(),
        socket_path.display()
    );
    let mut command = tokio::process::Command::new(&helper_path);
    command.arg("--socket").arg(&socket_path);
    if with_audio {
        command.arg("--audio");
    }
    let helper = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn();
    let mut helper = match helper {
        Ok(h) => h,
        Err(e) => {
            crate::diag!("[screenshare] spawn {}: {e}", helper_path.display());
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
                crate::diag!("[screenshare] helper connected");
                r
            }
            Err(e) => {
                crate::diag!("[screenshare] accept: {e}");
                let _ = std::fs::remove_file(&socket_path);
                return Err(fail_capture(
                    state,
                    "Screen-capture helper failed to connect. Please try again.".into(),
                )
                .await);
            }
        },
        status = helper.wait() => {
            crate::diag!("[screenshare] helper exited before connecting: {status:?}");
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

/// The helper binary name, shipped as a Tauri `externalBin` sidecar.
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

pub(crate) fn locate_capture_helper() -> Result<std::path::PathBuf> {
    use std::path::PathBuf;

    let helper = capture_helper_name();

    // 1. Explicit override, for non-standard dev layouts.
    if let Ok(p) = std::env::var("POLLIS_CAPTURE_BIN") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Ok(p);
        }
    }

    // 2. Sidecar next to the executable (production).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(helper);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    // 3. Dev: the workspace target dir, the running profile first.
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
    // Also `target/<profile>` relative to CWD (`pnpm dev` from the repo root).
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
