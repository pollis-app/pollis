//! The minimum-version check in the real `pollis` binary (#1611): a blocked build prints what
//! happened and how to update, exits [`EXIT_UPGRADE_REQUIRED`], and never starts the UI —
//! interactive (a pty) and not. The policy comes from `POLLIS_CLIENT_POLICY_URL`, here `inline:`
//! JSON or an unreachable URL, and the data dir is a temp dir.

use std::path::Path;
use std::process::{Command, Output, Stdio};

const EXIT_UPGRADE_REQUIRED: i32 = 3;
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn floor_above_this_build() -> String {
    format!(r#"inline:{{"v":1,"minimum":{{"cli":"{}.0.0"}},"message":"Retired for testing."}}"#, major() + 1)
}

fn major() -> u64 {
    VERSION.split('.').next().unwrap().parse().unwrap()
}

const OPEN: &str = r#"inline:{"v":1,"minimum":{}}"#;
const UNREACHABLE: &str = "http://127.0.0.1:9/client-policy.json";

fn pollis(data_dir: &Path, policy_url: &str) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pollis"));
    cmd.env_clear()
        .env("HOME", data_dir)
        .env("POLLIS_DATA_DIR", data_dir)
        .env("R2_S3_ENDPOINT", "https://r2.invalid")
        .env("R2_PUBLIC_URL", "https://media.invalid")
        .env("POLLIS_DELIVERY_URL", "http://127.0.0.1:9")
        .env("POLLIS_CLIENT_POLICY_URL", policy_url);
    cmd
}

fn run_without_terminal(data_dir: &Path, policy_url: &str) -> Output {
    pollis(data_dir, policy_url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run pollis")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn non_interactive_a_blocked_build_says_so_and_exits_distinctly() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_without_terminal(dir.path(), &floor_above_this_build());
    assert_eq!(out.status.code(), Some(EXIT_UPGRADE_REQUIRED), "{out:?}");
    let stderr = text(&out.stderr);
    assert!(stderr.contains(&format!("this version ({VERSION}) is no longer supported")), "{stderr}");
    assert!(stderr.contains(&format!("Update to {}.0.0 or later", major() + 1)), "{stderr}");
    assert!(stderr.contains("Retired for testing."), "{stderr}");
    assert!(stderr.contains("cli-install.sh"), "{stderr}");
    // It stopped before the terminal check.
    assert!(!stderr.contains("not a terminal"), "{stderr}");
}

#[test]
fn the_block_is_cached_and_a_fresh_policy_lifts_it() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(run_without_terminal(dir.path(), &floor_above_this_build()).status.code(), Some(EXIT_UPGRADE_REQUIRED));
    // Offline: the cached block holds.
    let out = run_without_terminal(dir.path(), UNREACHABLE);
    assert_eq!(out.status.code(), Some(EXIT_UPGRADE_REQUIRED), "{out:?}");
    // A fresh policy with no minimum lifts it: the client goes on to its terminal check.
    let out = run_without_terminal(dir.path(), OPEN);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(text(&out.stderr).contains("not a terminal"));
    // And the cache now says allowed, offline too.
    let out = run_without_terminal(dir.path(), UNREACHABLE);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
}

#[test]
fn unreachable_with_no_cache_fails_open() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_without_terminal(dir.path(), UNREACHABLE);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(text(&out.stderr).contains("not a terminal"), "{out:?}");
}

#[test]
fn help_and_version_still_work_on_a_blocked_build() {
    let dir = tempfile::tempdir().unwrap();
    for flag in ["--version", "--help"] {
        let out = pollis(dir.path(), &floor_above_this_build())
            .arg(flag)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "{flag}: {out:?}");
    }
}

/// Interactive: on a real terminal the UI never starts (no alternate screen, no raw mode).
#[cfg(unix)]
#[test]
fn interactive_a_blocked_build_never_starts_the_ui() {
    use std::io::Read;
    use std::os::fd::{FromRawFd, OwnedFd};

    let dir = tempfile::tempdir().unwrap();
    let (mut master, mut slave) = (0, 0);
    // SAFETY: openpty fills two fds; both are owned below.
    let rc = unsafe {
        libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
    };
    assert_eq!(rc, 0, "openpty");
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let mut child = pollis(dir.path(), &floor_above_this_build())
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap();
    // Read while it runs: macOS drops what is buffered once the child side closes.
    let reader = std::thread::spawn(move || {
        let mut file = std::fs::File::from(master);
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match file.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            if buf.windows(18).any(|w| w == b"https://pollis.com") {
                break;
            }
        }
        buf
    });
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(EXIT_UPGRADE_REQUIRED));
    let buf = reader.join().unwrap();
    let screen = text(&buf);
    assert!(screen.contains("is no longer supported"), "{screen}");
    // `\x1b[?1049h` is the alternate screen the UI enters first.
    assert!(!screen.contains("\x1b[?1049h"), "the UI started: {screen:?}");
}
