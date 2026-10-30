//! The minimum-version check in the real `pollis` binary (#1611).
//!
//! - Interactive (a pty): with no usable cache, launch waits for the policy; a blocked build
//!   prints what happened and how to update, exits [`EXIT_UPGRADE_REQUIRED`] and never starts
//!   the UI. With a cache that allows it, the UI starts at once and the check runs behind it; a
//!   block found then leaves the UI the same way.
//! - Non-interactive: no fetch is paid for before the usage error, but a cached block still
//!   reports itself and exits 3.
//!
//! The policy comes from `POLLIS_CLIENT_POLICY_URL_OVERRIDE` pointing at a local server whose
//! answer the test switches; the data dir is a temp dir; the DS URL is non-prod, so the
//! override is honoured.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const EXIT_UPGRADE_REQUIRED: i32 = 3;
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn major() -> u64 {
    VERSION.split('.').next().unwrap().parse().unwrap()
}

fn floor_above_this_build() -> String {
    format!(r#"{{"v":1,"minimum":{{"cli":"{}.0.0"}},"message":"Retired for testing."}}"#, major() + 1)
}

const OPEN: &str = r#"{"v":1,"minimum":{}}"#;

/// What the policy server answers: status, body, and a delay before answering.
struct Reply {
    status: u16,
    body: String,
    delay: Duration,
}

/// A tiny HTTP server for the policy file, answering whatever `reply` holds, counting requests.
struct PolicyServer {
    url: String,
    reply: Arc<Mutex<Reply>>,
    hits: Arc<AtomicUsize>,
}

impl PolicyServer {
    fn start(status: u16, body: &str) -> PolicyServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/client-policy.json", listener.local_addr().unwrap());
        let reply = Arc::new(Mutex::new(Reply { status, body: body.to_string(), delay: Duration::ZERO }));
        let hits = Arc::new(AtomicUsize::new(0));
        let (r, h) = (Arc::clone(&reply), Arc::clone(&hits));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (r, h) = (Arc::clone(&r), Arc::clone(&h));
                std::thread::spawn(move || {
                    let mut stream = stream;
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    // Request line and headers.
                    while reader.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
                        if line == "\r\n" {
                            break;
                        }
                        line.clear();
                    }
                    h.fetch_add(1, Ordering::SeqCst);
                    let (status, body, delay) = {
                        let r = r.lock().unwrap();
                        (r.status, r.body.clone(), r.delay)
                    };
                    std::thread::sleep(delay);
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                });
            }
        });
        PolicyServer { url, reply, hits }
    }

    fn set(&self, status: u16, body: &str, delay: Duration) {
        *self.reply.lock().unwrap() = Reply { status, body: body.to_string(), delay };
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

fn pollis(data_dir: &Path, policy_url: &str) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pollis"));
    cmd.env_clear()
        .env("HOME", data_dir)
        .env("POLLIS_DATA_DIR", data_dir)
        .env("R2_S3_ENDPOINT", "https://r2.invalid")
        .env("R2_PUBLIC_URL", "https://media.invalid")
        .env("POLLIS_DELIVERY_URL", "http://127.0.0.1:9")
        .env("POLLIS_CLIENT_POLICY_URL_OVERRIDE", policy_url);
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

/// `pollis` on a pty, with a thread collecting everything it writes to the terminal.
#[cfg(unix)]
struct OnTerminal {
    child: Child,
    screen: Arc<Mutex<Vec<u8>>>,
}

#[cfg(unix)]
impl OnTerminal {
    fn spawn(data_dir: &Path, policy_url: &str) -> OnTerminal {
        use std::os::fd::{FromRawFd, OwnedFd};
        let (mut master, mut slave) = (0, 0);
        // SAFETY: openpty fills two fds; both are owned below.
        let rc = unsafe {
            libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
        };
        assert_eq!(rc, 0, "openpty");
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        let child = pollis(data_dir, policy_url)
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave))
            .spawn()
            .unwrap();
        // Read while it runs: macOS drops what is buffered once the child side closes.
        let screen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&screen);
        std::thread::spawn(move || {
            let mut file = std::fs::File::from(master);
            let mut chunk = [0u8; 4096];
            while let Ok(n) = file.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        });
        OnTerminal { child, screen }
    }

    fn screen(&self) -> String {
        text(&self.screen.lock().unwrap())
    }

    /// Wait for `needle` on the screen, up to `within`.
    fn shows(&self, needle: &str, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if self.screen().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn wait_exit(&mut self, within: Duration) -> Option<i32> {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }
}

#[cfg(unix)]
impl Drop for OnTerminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The alternate screen the UI enters first.
const ALT_SCREEN: &str = "\x1b[?1049h";

#[test]
fn non_interactive_pays_for_no_fetch_before_its_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let server = PolicyServer::start(200, &floor_above_this_build());
    let out = run_without_terminal(dir.path(), &server.url);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(text(&out.stderr).contains("not a terminal"));
    assert_eq!(server.hits(), 0, "no fetch before the usage error");
}

#[cfg(unix)]
#[test]
fn interactive_a_blocked_build_never_starts_the_ui_and_the_block_is_cached() {
    let dir = tempfile::tempdir().unwrap();
    let server = PolicyServer::start(200, &floor_above_this_build());

    let mut term = OnTerminal::spawn(dir.path(), &server.url);
    assert_eq!(term.wait_exit(Duration::from_secs(20)), Some(EXIT_UPGRADE_REQUIRED), "{}", term.screen());
    assert!(term.shows("https://pollis.com", Duration::from_secs(2)));
    let screen = term.screen();
    assert!(screen.contains(&format!("this version ({VERSION}) is no longer supported")), "{screen}");
    assert!(screen.contains(&format!("Update to {}.0.0 or later", major() + 1)), "{screen}");
    assert!(screen.contains("Retired for testing."), "{screen}");
    assert!(screen.contains("cli-install.sh"), "{screen}");
    assert!(!screen.contains(ALT_SCREEN), "the UI started: {screen:?}");

    // Non-interactive now reports the cached block, still without a fetch.
    let hits = server.hits();
    let out = run_without_terminal(dir.path(), &server.url);
    assert_eq!(out.status.code(), Some(EXIT_UPGRADE_REQUIRED), "{out:?}");
    assert!(text(&out.stderr).contains("is no longer supported"));
    assert_eq!(server.hits(), hits);

    // The cache answers only for the source it came from.
    let out = run_without_terminal(dir.path(), "http://127.0.0.1:9/elsewhere.json");
    assert_eq!(out.status.code(), Some(1), "{out:?}");

    // Offline (the server errors), interactive: the cached block holds.
    server.set(500, "", Duration::ZERO);
    let mut term = OnTerminal::spawn(dir.path(), &server.url);
    assert_eq!(term.wait_exit(Duration::from_secs(20)), Some(EXIT_UPGRADE_REQUIRED), "{}", term.screen());
}

/// A cache that allows this build starts the UI at once, however slow the policy server is; a
/// block found by the background check then leaves the UI with the same message and exit code.
#[cfg(unix)]
#[test]
fn an_allowing_cache_starts_at_once_and_a_background_block_still_lands() {
    let dir = tempfile::tempdir().unwrap();
    let server = PolicyServer::start(200, OPEN);
    // First launch: no cache, so it waits for the fetch (allowed), caches it, and starts.
    {
        let term = OnTerminal::spawn(dir.path(), &server.url);
        assert!(term.shows(ALT_SCREEN, Duration::from_secs(20)), "{:?}", term.screen());
    }

    // Now the server is slow and says blocked. Launch must not wait for it.
    server.set(200, &floor_above_this_build(), Duration::from_secs(3));
    let started = Instant::now();
    let mut term = OnTerminal::spawn(dir.path(), &server.url);
    assert!(term.shows(ALT_SCREEN, Duration::from_secs(20)), "{:?}", term.screen());
    assert!(started.elapsed() < Duration::from_secs(3), "launch waited for the fetch: {:?}", started.elapsed());
    // The background check comes back blocked: the UI leaves and says why.
    assert_eq!(term.wait_exit(Duration::from_secs(20)), Some(EXIT_UPGRADE_REQUIRED), "{:?}", term.screen());
    assert!(term.shows("is no longer supported", Duration::from_secs(2)), "{:?}", term.screen());
}

#[test]
fn unreachable_with_no_cache_fails_open_and_inline_is_never_cached() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_without_terminal(dir.path(), "http://127.0.0.1:9/client-policy.json");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let out = run_without_terminal(dir.path(), &format!("inline:{}", floor_above_this_build()));
    assert_eq!(out.status.code(), Some(1), "an inline policy is not consulted without a terminal: {out:?}");
    assert!(!dir.path().join("client-policy.json").exists());
}

#[test]
fn help_and_version_stay_instant_and_never_fetch() {
    let dir = tempfile::tempdir().unwrap();
    let server = PolicyServer::start(200, &floor_above_this_build());
    for flag in ["--version", "--help"] {
        let out = pollis(dir.path(), &server.url).arg(flag).stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "{flag}: {out:?}");
    }
    assert_eq!(server.hits(), 0);
}
