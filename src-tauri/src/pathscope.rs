//! The set of filesystem paths the renderer is allowed to name.
//! The rule this module enforces is: **the renderer may only name a path a
//! human physically chose in an OS file dialog, or dropped onto the window.**
//! Those are the two ways a path legitimately enters the UI, and both of them
//! already involve the operating system's own consent gesture.
//!
//! An entry therefore exists if and only if a human pointed at that path. The
//! renderer can lie about anything it likes; it cannot lie this set larger.
//!
//! Not Tauri's `fs::Scope`: it canonicalizes, so it rejects every save target (which does not exist yet).

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use pollis_core::error::{Error, Result};

/// Every path a human chose this session; deliberately not persisted.
#[derive(Default)]
pub struct PathScope {
    /// Exact paths: a file that was picked, dropped, or named in a save dialog.
    files: Mutex<HashSet<PathBuf>>,
    /// Directory prefixes: a picked folder, and the parent of a save target.
    dirs: Mutex<HashSet<PathBuf>>,
}

/// Lexical normalize (save targets don't exist yet); `None` if `..` climbs above the root.
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
    /// Record a picked file; not its parent directory.
    pub fn remember_file(&self, path: &Path) {
        if let Some(path) = normalize(path) {
            self.files.lock().expect("path scope").insert(path);
        }
    }

    pub fn remember_dir(&self, path: &Path) {
        if let Some(path) = normalize(path) {
            self.dirs.lock().expect("path scope").insert(path);
        }
    }

    /// Record a save target and its parent dir (for sidecars like `files/`).
    pub fn remember_save_target(&self, path: &Path) {
        self.remember_file(path);
        if let Some(parent) = path.parent() {
            if parent.as_os_str().is_empty() {
                return;
            }
            self.remember_dir(parent);
        }
    }

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

    /// The error never echoes the path, or it becomes an existence oracle.
    pub fn require(&self, path: &Path, what: &str) -> Result<()> {
        if self.is_allowed(path) {
            return Ok(());
        }
        Err(Error::Other(anyhow::anyhow!(
            "{what}: that location was not chosen in a file dialog on this device"
        )))
    }
}

/// Record paths the OS dropped on the window; never called from a command.
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

    #[test]
    fn a_save_target_that_does_not_exist_yet_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("pollis-export.json");
        assert!(!target.exists());

        let scope = PathScope::default();
        scope.remember_save_target(&target);
        assert!(scope.is_allowed(&target));
        assert!(scope.is_allowed(&dir.path().join("pollis-export-files")));
    }

    /// `..` must collapse before the prefix check, or one dir grants the whole filesystem.
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
        assert!(!scope.is_allowed(&dir.path().join("not-dropped.png")));
    }
}
