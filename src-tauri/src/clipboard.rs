//! OS clipboard reads for the composer's paste fallback (#1794).
//!
//! Every read here can wait on another process: on X11 the clipboard owner
//! answers our request whenever it likes, or never. So nothing in this module
//! may run on the main thread (that is the GTK/WebKit UI thread on Linux), and
//! every read is bounded by a deadline the caller can't forget: `off_main`.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Most files one paste may name; the composer caps attachments well below this.
pub const MAX_PASTED_FILES: usize = 32;

/// Largest clipboard image we decode and re-encode (an 8K frame is ~33 MP).
pub const MAX_IMAGE_PIXELS: u64 = 64 * 1024 * 1024;

/// Run a blocking clipboard read on the blocking pool, giving up after `limit`.
/// A read that outlives `limit` keeps its blocking thread until the owner
/// answers or the library's own timeout fires, but nobody waits on it.
pub async fn off_main<T, F>(limit: Duration, read: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> Option<T> + Send + 'static,
{
    let task = tauri::async_runtime::spawn_blocking(read);
    match tokio::time::timeout(limit, task).await {
        Ok(Ok(value)) => value,
        Ok(Err(_)) => None,
        Err(_) => None,
    }
}

/// Lines GNOME Files (and Nemo, Caja) put above the URIs in its `text/plain` copy.
fn is_file_manager_header(line: &str) -> bool {
    matches!(
        line,
        "x-special/nautilus-clipboard" | "x-special/gnome-copied-files" | "copy" | "cut"
    )
}

/// Local paths from a `text/uri-list` (RFC 2483), tolerating a file manager's header lines.
/// Callers pass only a typed file-list target, never clipboard plain text: a web page can
/// put `file:///home/you/.ssh/id_ed25519` on the clipboard as text, and these paths are
/// admitted to `pathscope`. Strict as a second line of defence: one line that is not a
/// local `file:` URI means this was not a file copy, and nothing is returned.
pub fn paths_from_uri_list(bytes: &[u8]) -> Vec<PathBuf> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for raw in text.split(['\n', '\r', '\0']) {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || is_file_manager_header(line) {
            continue;
        }
        let Ok(url) = url::Url::parse(line) else {
            return Vec::new();
        };
        if url.scheme() != "file" {
            return Vec::new();
        }
        // A `file://otherhost/...` URI names a file on another machine.
        if url.host_str().is_some_and(|h| !h.is_empty() && h != "localhost") {
            return Vec::new();
        }
        let Ok(path) = url.to_file_path() else {
            return Vec::new();
        };
        paths.push(path);
        if paths.len() > MAX_PASTED_FILES {
            return Vec::new();
        }
    }
    paths
}

/// The pasted paths the composer may attach: existing regular files only.
/// Folders are refused (the composer can't send one) and so are dangling URIs.
pub fn admissible_files(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .filter(|p| p.is_absolute() && is_regular_file(p))
        .take(MAX_PASTED_FILES)
        .collect()
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
}

/// Whether an image of these dimensions is small enough to decode and re-encode.
pub fn image_within_cap(width: u64, height: u64) -> bool {
    width > 0 && height > 0 && width.saturating_mul(height) <= MAX_IMAGE_PIXELS
}

/// True when this process talks to an X server rather than a Wayland compositor:
/// the case where clipboard owners are reached directly and may never answer.
#[cfg(target_os = "linux")]
pub fn is_x11_session() -> bool {
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    let gdk_x11 = std::env::var("GDK_BACKEND").is_ok_and(|v| v.starts_with("x11"));
    set("DISPLAY") && (gdk_x11 || !set("WAYLAND_DISPLAY"))
}

/// A small, bounded X11 selection reader.
///
/// Why not `arboard` here: it insists the reply's property type equal the
/// target it asked for, and Steam answers a `text/uri-list` request with the
/// data typed `UTF8_STRING` (ValveSoftware/steam-for-linux#11476), so arboard
/// throws a valid file list away. It also tries six text targets in turn with
/// a 4 s wait each, so an owner that ignores unknown targets costs 24 s. This
/// reader asks for `TARGETS` first and only requests what the owner offers.
#[cfg(target_os = "linux")]
pub mod x11 {
    use std::time::{Duration, Instant};

    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        Atom, AtomEnum, ConnectionExt, CreateWindowAux, EventMask, GetPropertyReply, Property,
        WindowClass,
    };
    use x11rb::protocol::Event;
    use x11rb::rust_connection::RustConnection;
    use x11rb::NONE;

    /// File lists are short; anything bigger is not one.
    const MAX_FILE_LIST_BYTES: u32 = 256 * 1024;
    /// Generous for a paste into a text field.
    const MAX_TEXT_BYTES: u32 = 16 * 1024 * 1024;
    /// Text targets in order of preference; `STRING` is Latin-1 (ICCCM).
    const TEXT_TARGETS: [&str; 4] = ["UTF8_STRING", "text/plain;charset=utf-8", "STRING", "text/plain"];

    pub struct Reader {
        conn: RustConnection,
        window: u32,
        clipboard: Atom,
        property: Atom,
        targets: Atom,
        incr: Atom,
        deadline: Instant,
    }

    impl Reader {
        /// Open a private connection and an unmapped window to receive replies on.
        pub fn connect(limit: Duration) -> Option<Self> {
            let (conn, screen_num) = x11rb::connect(None).ok()?;
            let root = conn.setup().roots.get(screen_num)?.root;
            let window = conn.generate_id().ok()?;
            conn.create_window(
                0,
                window,
                root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                0,
                &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )
            .ok()?;
            let clipboard = intern(&conn, "CLIPBOARD")?;
            let property = intern(&conn, "POLLIS_CLIPBOARD")?;
            let targets = intern(&conn, "TARGETS")?;
            let incr = intern(&conn, "INCR")?;
            Some(Self {
                conn,
                window,
                clipboard,
                property,
                targets,
                incr,
                deadline: Instant::now() + limit,
            })
        }

        pub fn atom(&self, name: &str) -> Option<Atom> {
            intern(&self.conn, name)
        }

        /// The targets the CLIPBOARD owner offers; empty when there is no owner.
        pub fn offered(&self) -> Vec<Atom> {
            let owner = self
                .conn
                .get_selection_owner(self.clipboard)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| r.owner)
                .unwrap_or(NONE);
            if owner == NONE {
                return Vec::new();
            }
            let Some((_, bytes)) = self.convert(self.targets, 64 * 1024) else {
                return Vec::new();
            };
            bytes
                .chunks_exact(4)
                .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        }

        /// Ask the owner to convert CLIPBOARD to `target`; returns (type, bytes).
        /// Accepts whatever type the owner writes: callers validate the bytes.
        /// Gives up at the deadline, on a refusal, or past `max_bytes` (INCR included).
        pub fn convert(&self, target: Atom, max_bytes: u32) -> Option<(Atom, Vec<u8>)> {
            self.conn.delete_property(self.window, self.property).ok()?;
            self.conn
                .convert_selection(self.window, self.clipboard, target, self.property, x11rb::CURRENT_TIME)
                .ok()?;
            self.conn.flush().ok()?;
            let notify = loop {
                match self.next_event()? {
                    Event::SelectionNotify(n) if n.requestor == self.window && n.target == target => break n,
                    _ => continue,
                }
            };
            if notify.property == NONE {
                return None;
            }
            // Deleting the property (`true`) is also INCR's "send the next chunk".
            let reply = self.take_property(max_bytes)?;
            if reply.type_ == self.incr {
                return self.read_incr(max_bytes);
            }
            Some((reply.type_, reply.value))
        }

        /// ICCCM INCR: the owner writes chunks; each delete asks for the next; an empty one ends it.
        fn read_incr(&self, max_bytes: u32) -> Option<(Atom, Vec<u8>)> {
            let mut data = Vec::new();
            loop {
                match self.next_event()? {
                    Event::PropertyNotify(p)
                        if p.window == self.window && p.atom == self.property && p.state == Property::NEW_VALUE => {}
                    _ => continue,
                }
                let chunk = self.take_property(max_bytes)?;
                if chunk.value.is_empty() {
                    return Some((chunk.type_, data));
                }
                if data.len() + chunk.value.len() > max_bytes as usize {
                    return None;
                }
                data.extend_from_slice(&chunk.value);
            }
        }

        /// Read and delete our property; `None` when it holds more than `max_bytes`.
        fn take_property(&self, max_bytes: u32) -> Option<GetPropertyReply> {
            let reply = self
                .conn
                .get_property(true, self.window, self.property, AtomEnum::ANY, 0, max_bytes.div_ceil(4))
                .ok()?
                .reply()
                .ok()?;
            self.conn.flush().ok()?;
            (reply.bytes_after == 0).then_some(reply)
        }

        /// The next event, or `None` at the deadline: the owner never answered,
        /// the exact case that used to freeze the window.
        fn next_event(&self) -> Option<Event> {
            while Instant::now() < self.deadline {
                if let Some(event) = self.conn.poll_for_event().ok()? {
                    return Some(event);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            None
        }
    }

    impl Drop for Reader {
        fn drop(&mut self) {
            let _ = self.conn.destroy_window(self.window);
            let _ = self.conn.flush();
        }
    }

    fn intern(conn: &RustConnection, name: &str) -> Option<Atom> {
        Some(conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok()?.atom)
    }

    /// What an X11 clipboard holds, read without ever asking for a target the owner did not offer.
    pub struct Offer {
        pub uri_list: Option<Vec<u8>>,
        pub has_png: bool,
    }

    /// Read the file list if one is offered, and note whether a PNG is.
    /// `want_files: false` only lists the targets (the image path's pre-check).
    pub fn read_offer(limit: Duration, want_files: bool) -> Option<Offer> {
        let reader = Reader::connect(limit)?;
        let offered = reader.offered();
        let atom_offered = |name: &str| reader.atom(name).filter(|a| offered.contains(a));
        let uri_list = if want_files {
            atom_offered("text/uri-list").and_then(|a| reader.convert(a, MAX_FILE_LIST_BYTES)).map(|(_, b)| b)
        } else {
            None
        };
        Some(Offer { uri_list, has_png: atom_offered("image/png").is_some() })
    }

    /// The clipboard's text, from the first text target the owner offers; `None` if it offers none.
    /// This replaces WebKitGTK's own paste on X11, which asks every owner for targets it
    /// never offered and waits ~30 s on each one an owner like Steam leaves unanswered.
    pub fn read_text(limit: Duration) -> Option<String> {
        let reader = Reader::connect(limit)?;
        let offered = reader.offered();
        let (name, atom) = TEXT_TARGETS
            .iter()
            .find_map(|name| reader.atom(name).filter(|a| offered.contains(a)).map(|a| (*name, a)))?;
        let (_, bytes) = reader.convert(atom, MAX_TEXT_BYTES)?;
        Some(super::decode_text(name == "STRING", &bytes))
    }
}

/// Clipboard text bytes as a string: `latin1` for X11's `STRING`, else UTF-8 (lossy).
/// A trailing NUL, which some owners append, is not part of the text.
pub fn decode_text(latin1: bool, bytes: &[u8]) -> String {
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    if latin1 {
        bytes.iter().map(|&b| char::from(b)).collect()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unix paths: `file:///tmp/..` has no drive letter on Windows.
    #[cfg(unix)]
    #[test]
    fn a_steam_screenshot_uri_list_yields_its_path() {
        // Byte-for-byte the shape Steam writes: one URI, CRLF-terminated.
        let bytes = b"file:///home/someone/.steam/debian-installation/userdata/1/760/remote/570940/screenshots/20260522171713_1.jpg\r\n";
        assert_eq!(
            paths_from_uri_list(bytes),
            vec![PathBuf::from(
                "/home/someone/.steam/debian-installation/userdata/1/760/remote/570940/screenshots/20260522171713_1.jpg"
            )]
        );
    }

    // Unix paths: `file:///tmp/..` has no drive letter on Windows.
    #[cfg(unix)]
    #[test]
    fn percent_escapes_comments_and_several_files() {
        let bytes = b"# copied\r\nfile:///tmp/a%20b.png\r\nfile://localhost/tmp/c.png\r\n";
        assert_eq!(
            paths_from_uri_list(bytes),
            vec![PathBuf::from("/tmp/a b.png"), PathBuf::from("/tmp/c.png")]
        );
    }

    // Unix paths: `file:///tmp/..` has no drive letter on Windows.
    #[cfg(unix)]
    #[test]
    fn a_file_manager_plain_text_copy_is_a_file_list() {
        let bytes = b"x-special/nautilus-clipboard\ncopy\nfile:///tmp/a.png\n";
        assert_eq!(paths_from_uri_list(bytes), vec![PathBuf::from("/tmp/a.png")]);
    }

    #[test]
    fn text_that_merely_contains_a_file_uri_attaches_nothing() {
        assert!(paths_from_uri_list(b"see file:///home/someone/.ssh/id_ed25519").is_empty());
        assert!(paths_from_uri_list(b"file:///tmp/a.png\nhttps://example.com/b.png\n").is_empty());
        assert!(paths_from_uri_list(b"just some words").is_empty());
        assert!(paths_from_uri_list(b"file://otherhost/tmp/a.png").is_empty());
        assert!(paths_from_uri_list(&[0xff, 0xfe, 0x00]).is_empty());
        assert!(paths_from_uri_list(b"").is_empty());
    }

    #[test]
    fn an_enormous_list_is_refused_not_truncated() {
        let many: String = (0..=MAX_PASTED_FILES).map(|i| format!("file:///tmp/{i}.png\n")).collect();
        assert!(paths_from_uri_list(many.as_bytes()).is_empty());
    }

    #[test]
    fn only_existing_regular_files_are_admitted() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("shot.jpg");
        std::fs::write(&file, b"jpg").unwrap();
        let missing = dir.path().join("gone.jpg");
        let admitted = admissible_files(vec![
            file.clone(),
            dir.path().to_path_buf(),
            missing,
            PathBuf::from("relative.jpg"),
        ]);
        assert_eq!(admitted, vec![file]);
    }

    #[test]
    fn image_cap_refuses_empty_and_huge_frames() {
        assert!(image_within_cap(3840, 2160));
        assert!(image_within_cap(7680, 4320));
        assert!(!image_within_cap(0, 100));
        assert!(!image_within_cap(100_000, 100_000));
        assert!(!image_within_cap(u64::MAX, u64::MAX));
    }

    #[test]
    fn clipboard_text_decodes_utf8_and_latin1() {
        assert_eq!(decode_text(false, "héllo ✓".as_bytes()), "héllo ✓");
        assert_eq!(decode_text(true, &[b'h', 0xe9, b'l']), "hél");
        assert_eq!(decode_text(false, b"text\0"), "text");
        assert_eq!(decode_text(false, &[b'a', 0xff, b'b']), "a\u{fffd}b");
    }

    #[tokio::test]
    async fn a_read_that_never_returns_gives_up_at_the_limit() {
        let started = std::time::Instant::now();
        let out: Option<u8> = off_main(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_secs(2));
            Some(1)
        })
        .await;
        assert_eq!(out, None);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn a_read_that_returns_in_time_is_passed_through() {
        assert_eq!(off_main(Duration::from_secs(2), || Some(7u8)).await, Some(7));
    }
}
