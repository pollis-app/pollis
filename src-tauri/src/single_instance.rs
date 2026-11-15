//! One running app per installed binary, OS user and data dir (#1923).
//!
//! A second launch of the same binary hands its arguments to the running
//! instance and exits before any window, database, keystore or LiveKit
//! connection exists; the running instance brings its window forward. A copy of
//! the binary at another path (a second install, a `pnpm dev` or test build, an
//! AppImage elsewhere), another OS user, or another `POLLIS_DATA_DIR` (the e2e
//! harness runs two clients that way) gets a different key and runs alongside.
//!
//! Not `tauri-plugin-single-instance`: it keys on the bundle identifier (a
//! `/tmp` socket on macOS shared by every user, a session mutex on Windows), so
//! a dev build and the installed app would block each other.
//!
//! Mechanism: an exclusive OS file lock on `pollis-<key>.lock` in a per-user
//! directory decides who is primary — the kernel drops it if the process dies,
//! so a crash leaves nothing stale. The primary listens on a Unix socket
//! (`pollis-<key>.sock`, same directory) or a named pipe (`\\.\pipe\pollis-<key>`);
//! a secondary connects, writes its arguments and exits. Every failure fails
//! open: the app starts normally rather than refusing to run.

use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;
use std::time::Duration;

/// A hand-off larger than this is not a launch's argument list; read no further.
const MAX_MESSAGE: u64 = 64 * 1024;

/// How long a secondary keeps trying to reach a primary that holds the lock but
/// has not yet opened its listener (the gap is the few instructions between the
/// two). A bounded startup handshake, not polling: it ends at the first success.
const CONNECT_ATTEMPTS: u32 = 40;
const CONNECT_BACKOFF: Duration = Duration::from_millis(25);

/// The outcome of [`claim`].
pub enum Claim {
    /// This process is the instance; keep the guard alive for the app's lifetime.
    Primary(InstanceGuard),
    /// Another instance holds the lock and has been sent this launch's arguments.
    Secondary,
    /// The lock could not be checked; run normally.
    Unavailable,
}

/// Holds the instance lock and listener; dropping it releases both.
pub struct InstanceGuard {
    #[cfg(unix)]
    socket: PathBuf,
    receiver: Mutex<Option<Receiver<Vec<String>>>>,
    // Declared last so it is released after the socket file is removed above.
    _lock: File,
}

impl tauri::Resource for InstanceGuard {}

impl InstanceGuard {
    /// Calls `on_launch` with each later launch's arguments (`argv[1..]`), on a
    /// thread of its own. Launches that arrived before this is called are delivered
    /// then.
    pub fn on_launch(&self, on_launch: impl Fn(Vec<String>) + Send + 'static) {
        let Some(receiver) = self.receiver.lock().ok().and_then(|mut r| r.take()) else {
            return;
        };
        std::thread::spawn(move || {
            for args in receiver {
                on_launch(args);
            }
        });
    }
}

#[cfg(unix)]
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        // Removed while the lock is still held, so a newer primary's socket is never touched.
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// The instance key: one per (binary, OS user, data dir). Hex, 32 chars.
pub fn instance_key(exe: &Path, user: &str, data_dir: &Path) -> String {
    let mut h = Sha256::new();
    h.update(b"pollis-single-instance-v1\0");
    h.update(exe.as_os_str().as_encoded_bytes());
    h.update(b"\0");
    h.update(user.as_bytes());
    h.update(b"\0");
    h.update(data_dir.as_os_str().as_encoded_bytes());
    hex::encode(&h.finalize()[..16])
}

/// The file the user launched: the `.AppImage` itself on Linux (its payload is
/// mounted at a fresh random path each run), otherwise the executable.
fn launched_binary() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        if let Some(appimage) = std::env::var_os("APPIMAGE").filter(|p| !p.is_empty()) {
            let path = PathBuf::from(appimage);
            return Some(std::fs::canonicalize(&path).unwrap_or(path));
        }
    }
    let exe = std::env::current_exe().ok()?;
    Some(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// The OS account: the numeric uid on Unix, `DOMAIN\user` on Windows.
fn os_user() -> String {
    #[cfg(unix)]
    {
        // SAFETY: getuid cannot fail and touches no memory.
        unsafe { libc::getuid() }.to_string()
    }
    #[cfg(windows)]
    {
        let domain = std::env::var("USERDOMAIN").unwrap_or_default();
        let user = std::env::var("USERNAME").unwrap_or_default();
        format!("{domain}\\{user}")
    }
}

/// A per-user directory for the lock and socket: `$XDG_RUNTIME_DIR` on Linux,
/// else the temp dir (per user on macOS and Windows; on a Linux box without a
/// runtime dir the key's uid keeps users apart).
fn runtime_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
            if dir.is_absolute() && dir.is_dir() {
                return dir;
            }
        }
    }
    std::env::temp_dir()
}

/// Decides whether this launch is the instance. On `Secondary` the running
/// instance has been told; the caller exits.
pub fn claim() -> Claim {
    let Some(exe) = launched_binary() else {
        return Claim::Unavailable;
    };
    let data_dir = pollis_core::db::local::dirs_path();
    let key = instance_key(&exe, &os_user(), &data_dir);
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    claim_in(&runtime_dir(), &key, &args)
}

/// [`claim`] against an explicit directory and key (tests use a temp dir).
pub fn claim_in(dir: &Path, key: &str, args: &[String]) -> Claim {
    let lock_path = dir.join(format!("pollis-{key}.lock"));
    let lock = match OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
    {
        Ok(f) => f,
        Err(e) => {
            pollis_core::diag!("[single-instance] cannot open the lock file, running anyway: {e}");
            return Claim::Unavailable;
        }
    };
    match try_lock(&lock) {
        Ok(true) => {}
        Ok(false) => {
            if let Err(e) = forward(dir, key, args) {
                pollis_core::diag!("[single-instance] could not reach the running instance: {e}");
            }
            return Claim::Secondary;
        }
        Err(e) => {
            pollis_core::diag!("[single-instance] cannot lock, running anyway: {e}");
            return Claim::Unavailable;
        }
    }
    let (tx, rx) = channel();
    #[cfg(unix)]
    let socket = socket_path(dir, key);
    #[cfg(unix)]
    if let Err(e) = listen(&socket, tx) {
        pollis_core::diag!(
            "[single-instance] cannot listen; later launches will not raise this one: {e}"
        );
    }
    #[cfg(windows)]
    listen(key, tx);
    Claim::Primary(InstanceGuard {
        #[cfg(unix)]
        socket,
        receiver: Mutex::new(Some(rx)),
        _lock: lock,
    })
}

/// Takes an exclusive lock without waiting: `Ok(false)` when another process
/// holds it. The OS releases it when the file closes or the process dies.
/// (`File::try_lock` would do, but it is newer than the workspace MSRV.) Unix
/// shares the state lock's `flock`; that one always grants on Windows (where
/// nothing contends the state lock), so Windows takes a real `LockFileEx` here.
#[cfg(unix)]
fn try_lock(file: &File) -> std::io::Result<bool> {
    pollis_core::notify::lock::try_lock_exclusive(file)
}

#[cfg(windows)]
fn try_lock(file: &File) -> std::io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    // SAFETY: an all-zero OVERLAPPED is valid (offset 0, no event); the handle
    // is owned by `file` for the call's duration.
    let ok = unsafe {
        let mut overlapped = std::mem::zeroed();
        LockFileEx(
            file.as_raw_handle() as _,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    };
    if ok != 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        Ok(false)
    } else {
        Err(err)
    }
}

fn encode(args: &[String]) -> Vec<u8> {
    args.join("\0").into_bytes()
}

fn decode(bytes: &[u8]) -> Vec<String> {
    if bytes.is_empty() {
        return Vec::new();
    }
    String::from_utf8_lossy(bytes)
        .split('\0')
        .map(String::from)
        .collect()
}

/// Reads one hand-off from `stream` and queues it.
#[cfg(unix)]
fn receive(stream: impl std::io::Read, tx: &Sender<Vec<String>>) -> bool {
    use std::io::Read;
    let mut buf = Vec::new();
    if stream.take(MAX_MESSAGE).read_to_end(&mut buf).is_err() {
        return true;
    }
    tx.send(decode(&buf)).is_ok()
}

/// Writes `args` to the running instance, retrying while it finishes starting.
fn forward(dir: &Path, key: &str, args: &[String]) -> std::io::Result<()> {
    let mut last = None;
    for _ in 0..CONNECT_ATTEMPTS {
        match connect(dir, key) {
            Ok(mut stream) => {
                stream.write_all(&encode(args))?;
                return stream.flush();
            }
            Err(e) => {
                last = Some(e);
                std::thread::sleep(CONNECT_BACKOFF);
            }
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no attempt made")))
}

/// Unix socket paths are capped at ~104 bytes; fall back to `/tmp` (the key
/// carries the uid) if the per-user dir is unusually deep.
#[cfg(unix)]
fn socket_path(dir: &Path, key: &str) -> PathBuf {
    let path = dir.join(format!("pollis-{key}.sock"));
    if path.as_os_str().len() < 100 {
        path
    } else {
        PathBuf::from(format!("/tmp/pollis-{key}.sock"))
    }
}

#[cfg(unix)]
fn connect(dir: &Path, key: &str) -> std::io::Result<std::os::unix::net::UnixStream> {
    std::os::unix::net::UnixStream::connect(socket_path(dir, key))
}

#[cfg(unix)]
fn listen(socket: &Path, tx: Sender<Vec<String>>) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    // We hold the lock, so any socket file here is a dead instance's leftover.
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            // A local client that connects and stalls must not wedge the listener.
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            if !receive(stream, &tx) {
                break;
            }
        }
    });
    Ok(())
}

#[cfg(windows)]
fn pipe_name(key: &str) -> String {
    format!(r"\\.\pipe\pollis-{key}")
}

#[cfg(windows)]
fn connect(_dir: &Path, key: &str) -> std::io::Result<File> {
    OpenOptions::new().write(true).open(pipe_name(key))
}

/// Serves the named pipe from its own thread; one instance per connection.
#[cfg(windows)]
fn listen(key: &str, tx: Sender<Vec<String>>) {
    use tokio::io::AsyncReadExt;
    use tokio::net::windows::named_pipe::ServerOptions;
    let name = pipe_name(key);
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                pollis_core::diag!("[single-instance] no runtime for the pipe: {e}");
                return;
            }
        };
        runtime.block_on(async move {
            loop {
                let server = match ServerOptions::new()
                    .reject_remote_clients(true)
                    .create(&name)
                {
                    Ok(s) => s,
                    Err(e) => {
                        pollis_core::diag!("[single-instance] cannot create the pipe: {e}");
                        return;
                    }
                };
                if server.connect().await.is_err() {
                    continue;
                }
                let read = async move {
                    let mut buf = Vec::new();
                    server
                        .take(MAX_MESSAGE)
                        .read_to_end(&mut buf)
                        .await
                        .map(|_| buf)
                };
                // A local client that connects and stalls must not wedge the listener.
                let Ok(Ok(buf)) = tokio::time::timeout(Duration::from_secs(2), read).await else {
                    continue;
                };
                if tx.send(decode(&buf)).is_err() {
                    return;
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_for(exe: &str, user: &str, data: &str) -> String {
        instance_key(Path::new(exe), user, Path::new(data))
    }

    #[test]
    fn same_binary_user_and_data_dir_share_a_key() {
        let a = key_for(
            "/Applications/Pollis.app/Contents/MacOS/pollis",
            "501",
            "/d",
        );
        let b = key_for(
            "/Applications/Pollis.app/Contents/MacOS/pollis",
            "501",
            "/d",
        );
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn another_path_user_or_data_dir_gets_its_own_key() {
        let base = key_for("/opt/Pollis/pollis", "1000", "/home/a/.local/share/pollis");
        assert_ne!(
            base,
            key_for(
                "/home/a/dev/target/debug/pollis",
                "1000",
                "/home/a/.local/share/pollis"
            )
        );
        assert_ne!(
            base,
            key_for("/opt/Pollis/pollis", "1001", "/home/a/.local/share/pollis")
        );
        assert_ne!(
            base,
            key_for("/opt/Pollis/pollis", "1000", "/tmp/e2e-client-b")
        );
    }

    #[test]
    fn fields_cannot_bleed_into_each_other() {
        assert_ne!(key_for("/a", "1b", "/c"), key_for("/a1", "b", "/c"));
    }

    #[test]
    fn args_round_trip() {
        let args = vec!["--flag".to_string(), "a b".to_string(), String::new()];
        assert_eq!(decode(&encode(&args)), args);
        assert!(decode(&encode(&[])).is_empty());
    }

    fn unique_key(tag: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        format!("test-{tag}-{}-{nanos}", std::process::id())
    }

    #[test]
    fn a_second_launch_hands_its_args_to_the_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = unique_key("handoff");
        let Claim::Primary(guard) = claim_in(dir.path(), &key, &[]) else {
            panic!("first claim must be primary");
        };
        let (tx, rx) = channel();
        guard.on_launch(move |args| {
            let _ = tx.send(args);
        });
        let args = vec!["pollis://open".to_string()];
        assert!(matches!(
            claim_in(dir.path(), &key, &args),
            Claim::Secondary
        ));
        let got = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("hand-off delivered");
        assert_eq!(got, args);
    }

    #[test]
    fn a_launch_after_the_instance_exits_becomes_the_instance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = unique_key("release");
        let first = claim_in(dir.path(), &key, &[]);
        assert!(matches!(first, Claim::Primary(_)));
        drop(first);
        let Claim::Primary(guard) = claim_in(dir.path(), &key, &[]) else {
            panic!("the lock must be free once the first instance is gone");
        };
        let (tx, rx) = channel();
        guard.on_launch(move |args| {
            let _ = tx.send(args);
        });
        assert!(matches!(claim_in(dir.path(), &key, &[]), Claim::Secondary));
        // On Windows the dropped instance's pipe server lives until its process exits,
        // and this test keeps the process; Unix removes the old socket on drop.
        #[cfg(unix)]
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok());
        #[cfg(windows)]
        let _ = rx;
    }

    #[test]
    fn different_keys_do_not_block_each_other() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = claim_in(dir.path(), &unique_key("a"), &[]);
        let b = claim_in(dir.path(), &unique_key("b"), &[]);
        assert!(matches!(a, Claim::Primary(_)));
        assert!(matches!(b, Claim::Primary(_)));
    }
}
