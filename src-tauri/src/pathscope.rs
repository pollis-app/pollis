//! The set of filesystem paths the renderer is allowed to name.
//!
//! Four commands take a path or a directory as an IPC argument —
//! `upload_media`, `upload_group_emoji`, `export_archive` and
//! `fetch_export_attachments` — and each of them then reads or writes it with
//! the full authority of the app process. Nothing checked where the string came
//! from, so `invoke("upload_media", { path: "/home/you/.ssh/id_rsa", … })` read
//! the key and uploaded it, and `invoke("export_archive", { path: "…" })` wrote
//! a plaintext archive of every message anywhere on the disk. Any path that
//! reaches script execution inside the webview reaches both.
//!
//! The rule this module enforces is: **the renderer may only name a path a
//! human physically chose in an OS file dialog, or dropped onto the window.**
//! Those are the two ways a path legitimately enters the UI, and both of them
//! already involve the operating system's own consent gesture.
//!
//! ## Why the registry is written only by Rust
//!
//! A "call this bridge command right after the dialog to register the path"
//! design is worth nothing: a compromised renderer skips the dialog and calls
//! the register command directly. So no command registers a path on the
//! renderer's say-so. The two writers are both inside this process:
//!
//! * [`pick_open_paths`](crate::commands::pathscope::pick_open_paths) and
//!   [`pick_save_path`](crate::commands::pathscope::pick_save_path) — the
//!   renderer's *only* route to a file dialog. They drive
//!   `tauri_plugin_dialog`'s **Rust** API, so the path is produced by the OS
//!   picker inside this process and recorded before it is ever handed out. The
//!   `dialog:*` ACL permissions are removed from `capabilities/default.json`,
//!   so `plugin:dialog|open` is not reachable from the webview at all.
//! * [`remember_dropped`] — called from the `DragDrop` window-event handler in
//!   `run()`, with the paths the OS itself put in the event.
//!
//! An entry therefore exists if and only if a human pointed at that path. The
//! renderer can lie about anything it likes; it cannot lie this set larger.
//!
//! ## Why not Tauri's own scopes
//!
//! `tauri::fs::Scope` (which `tauri-plugin-dialog`'s IPC commands populate) is
//! the right idea but the wrong shape here: `Scope::is_allowed` canonicalizes
//! before matching, so it answers `false` for every **save** target, which by
//! definition does not exist yet. `export_archive` and "save attachment as…"
//! are exactly that case. This registry stores the path as the picker returned
//! it and compares it the same way, so a not-yet-created file is representable.
//!
//! Directories are stored as prefixes: `fetch_export_attachments` is handed the
//! `<archive>/files` subdirectory of a directory the user picked, and the save
//! of an archive legitimately creates children under the chosen folder.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use pollis_core::error::{Error, Result};

/// Managed state: every path a human has chosen this session.
///
/// Session-scoped on purpose. It is not persisted, so a restart forgets
/// everything and the user picks again — a grant that outlives the window it
/// was given in is a grant nobody remembers giving.
#[derive(Default)]
pub struct PathScope {
    /// Exact paths: a file that was picked, dropped, or named in a save dialog.
    files: Mutex<HashSet<PathBuf>>,
    /// Directory prefixes: a picked folder, and the parent of a save target.
    /// Anything at or below one of these is in scope.
    dirs: Mutex<HashSet<PathBuf>>,
}

/// Normalize without touching the filesystem.
///
/// `is_allowed`-style canonicalization is not usable here (save targets do not
/// exist yet), so the traversal defence has to be lexical: `.` segments are
/// dropped and `..` pops the previous segment, which means `<picked
/// dir>/../../etc/shadow` collapses to `/etc/shadow` and simply is not under
/// the prefix any more. A path that tries to climb above the root is rejected
/// outright by returning `None`.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    let mut depth: usize = 0;
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return None;
                }
                out.pop();
                depth -= 1;
            }
            Component::Prefix(_) | Component::RootDir => {
                out.push(component.as_os_str());
            }
            Component::Normal(part) => {
                out.push(part);
                depth += 1;
            }
        }
    }
    Some(out)
}

impl PathScope {
    /// Record a file the user chose. Its parent directory is NOT recorded — a
    /// pick of one file is consent for that file, not for its neighbours.
    pub fn remember_file(&self, path: &Path) {
        if let Some(path) = normalize(path) {
            self.files.lock().expect("path scope").insert(path);
        }
    }

    /// Record a directory the user chose, and everything beneath it.
    pub fn remember_dir(&self, path: &Path) {
        if let Some(path) = normalize(path) {
            self.dirs.lock().expect("path scope").insert(path);
        }
    }

    /// Record a save target: the file itself, and its parent as a directory so
    /// the sidecars a save legitimately creates beside it (an export's `files/`
    /// folder, say) are in scope too.
    pub fn remember_save_target(&self, path: &Path) {
        self.remember_file(path);
        if let Some(parent) = path.parent() {
            if parent.as_os_str().is_empty() {
                return;
            }
            self.remember_dir(parent);
        }
    }

    /// Whether `path` is one the user chose, or sits under a directory they
    /// chose.
    pub fn is_allowed(&self, path: &Path) -> bool {
        let Some(path) = normalize(path) else {
            return false;
        };
        if self.files.lock().expect("path scope").contains(&path) {
            return true;
        }
        self.dirs
            .lock()
            .expect("path scope")
            .iter()
            .any(|dir| path.starts_with(dir))
    }

    /// The gate the path-taking command shims call before forwarding.
    ///
    /// The error deliberately does not echo the path back: the renderer already
    /// knows what it asked for, and a refusal that quotes the argument turns
    /// this into an existence oracle for arbitrary filenames.
    pub fn require(&self, path: &Path, what: &str) -> Result<()> {
        if self.is_allowed(path) {
            return Ok(());
        }
        Err(Error::Other(anyhow::anyhow!(
            "{what}: that location was not chosen in a file dialog on this device"
        )))
    }
}

/// Record paths the OS dropped on the window. Called from `run()`'s window-event
/// handler, never from a command.
pub fn remember_dropped(scope: &PathScope, paths: &[PathBuf]) {
    for path in paths {
        if path.is_dir() {
            scope.remember_dir(path);
        } else {
            scope.remember_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_allowed_before_the_user_chooses_anything() {
        let scope = PathScope::default();
        assert!(!scope.is_allowed(Path::new("/home/someone/.ssh/id_rsa")));
        assert!(scope.require(Path::new("/etc/shadow"), "upload_media").is_err());
    }

    #[test]
    fn a_picked_file_is_allowed_and_its_siblings_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let picked = dir.path().join("holiday.png");
        let sibling = dir.path().join("id_rsa");
        let scope = PathScope::default();
        scope.remember_file(&picked);

        assert!(scope.is_allowed(&picked));
        assert!(!scope.is_allowed(&sibling));
        assert!(scope.require(&picked, "upload_media").is_ok());
        assert!(scope.require(&sibling, "upload_media").is_err());
    }

    #[test]
    fn a_picked_directory_covers_what_is_under_it() {
        let dir = tempfile::tempdir().unwrap();
        let chosen = dir.path().join("archive");
        let scope = PathScope::default();
        scope.remember_dir(&chosen);

        assert!(scope.is_allowed(&chosen));
        assert!(scope.is_allowed(&chosen.join("files")));
        assert!(scope.is_allowed(&chosen.join("files").join("a.png")));
        assert!(!scope.is_allowed(&dir.path().join("elsewhere")));
    }

    /// A save target does not exist yet. Anything that canonicalizes first
    /// (Tauri's own `fs::Scope`) answers `false` here, which is why this
    /// registry compares the path as the picker returned it.
    #[test]
    fn a_save_target_that_does_not_exist_yet_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("pollis-export.json");
        assert!(!target.exists());

        let scope = PathScope::default();
        scope.remember_save_target(&target);
        assert!(scope.is_allowed(&target));
        // …and the sidecar directory the export writes beside it.
        assert!(scope.is_allowed(&dir.path().join("pollis-export-files")));
    }

    /// The prefix check is lexical, so `..` must be collapsed before it is
    /// applied — otherwise one picked directory grants the whole filesystem.
    #[test]
    fn traversal_out_of_a_chosen_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let chosen = dir.path().join("archive");
        let scope = PathScope::default();
        scope.remember_dir(&chosen);

        let escape = chosen.join("..").join("..").join("..").join("etc").join("shadow");
        assert!(!scope.is_allowed(&escape));
        assert!(scope.require(&escape, "fetch_export_attachments").is_err());
    }

    /// …and the same collapse must not break the honest case: a path that
    /// wanders but lands back inside the chosen directory is still inside it.
    #[test]
    fn a_dotdot_that_stays_inside_is_still_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let chosen = dir.path().join("archive");
        let scope = PathScope::default();
        scope.remember_dir(&chosen);

        assert!(scope.is_allowed(&chosen.join("files").join("..").join("meta.json")));
    }

    #[test]
    fn a_relative_path_that_climbs_above_the_root_is_refused() {
        let scope = PathScope::default();
        scope.remember_dir(Path::new("/srv/pick"));
        assert!(!scope.is_allowed(Path::new("../../etc/shadow")));
    }

    #[test]
    fn dropped_files_and_directories_are_recorded_by_kind() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("dropped.png");
        std::fs::write(&file, b"x").unwrap();
        let subdir = dir.path().join("dropped-folder");
        std::fs::create_dir(&subdir).unwrap();

        let scope = PathScope::default();
        remember_dropped(&scope, &[file.clone(), subdir.clone()]);

        assert!(scope.is_allowed(&file));
        assert!(scope.is_allowed(&subdir.join("inner").join("x.png")));
        // A file drop does not open its parent directory.
        assert!(!scope.is_allowed(&dir.path().join("not-dropped.png")));
    }
}
