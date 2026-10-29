//! Owner-only filesystem creation, in one place.
//!
//! Every file this app writes belongs to one person; the umask default (0644) would expose it to other local users.
//!
//! The mode is not a parameter, so no caller can create a world-readable file.
//!
//! ## Platforms
//!
//! * **Unix** — 0600 files / 0700 dirs, passed to `open(2)`/`mkdir(2)` and re-applied via `fchmod` to tighten pre-existing files.
//! * **Windows** — nothing: `%APPDATA%` already inherits an owner-only ACL. See [`owner_only_is_enforced_by_mode`].

use std::io;
use std::path::Path;

/// Mode for every file this app creates.
#[cfg(unix)]
pub const FILE_MODE: u32 = 0o600;

/// Mode for every directory this app creates.
#[cfg(unix)]
pub const DIR_MODE: u32 = 0o700;

/// Unix enforces owner-only by explicit mode; Windows inherits it from the profile ACL.
pub const fn owner_only_is_enforced_by_mode() -> bool {
    cfg!(unix)
}

/// Create `dir` and any missing parents, owner-only.
///
/// The leaf mode is re-applied even if it exists; parents (the user's XDG tree) are left alone.
pub fn create_dir_all(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        use std::os::unix::fs::PermissionsExt;

        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(DIR_MODE)
            .create(dir)?;
        // `mkdir` mode only covers directories this call created.
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(DIR_MODE))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Create (or truncate) `path` for writing, owner-only.
pub fn create_file(path: &Path) -> io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(FILE_MODE);
    }
    let file = opts.open(path)?;
    restrict_open_handle(&file)?;
    Ok(file)
}

/// Write `contents` to `path`, creating it owner-only.
pub fn write(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    use std::io::Write;
    let mut file = create_file(path)?;
    file.write_all(contents.as_ref())
}

/// [`write`] off the async worker thread; cache files reach 100 MB.
pub async fn write_async(path: &Path, contents: Vec<u8>) -> io::Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || write(&path, &contents))
        .await
        .map_err(|e| io::Error::other(format!("private write task: {e}")))?
}

/// Tighten a file SQLite created (DB, `-wal`, `-shm`). A missing file is not an error.
pub fn restrict_existing(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::set_permissions(path, std::fs::Permissions::from_mode(FILE_MODE)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Apply [`FILE_MODE`] via `fchmod`; public for files opened elsewhere (e.g. `pollis-tui`'s append-mode log).
pub fn restrict_open_handle(file: &std::fs::File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(FILE_MODE))
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pollis-private-fs-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn mode_of(path: &Path) -> u32 {
        std::fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn created_files_are_owner_only() {
        // Predicate and behaviour asserted against each other, so a cfg branch cannot swallow the mode.
        assert!(
            owner_only_is_enforced_by_mode(),
            "this is the unix build, so the mode is what enforces owner-only here"
        );

        let dir = scratch("file");
        create_dir_all(&dir).unwrap();
        let path = dir.join("secret");
        write(&path, b"contents").unwrap();
        assert_eq!(mode_of(&path), FILE_MODE, "a new file must be 0600");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn created_directories_are_owner_only() {
        let dir = scratch("dir");
        create_dir_all(&dir.join("nested")).unwrap();
        assert_eq!(mode_of(&dir.join("nested")), DIR_MODE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_world_readable_file_is_tightened_on_rewrite() {
        let dir = scratch("rewrite");
        create_dir_all(&dir).unwrap();
        let path = dir.join("was-loose");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(mode_of(&path), 0o644);

        write(&path, b"new").unwrap();
        assert_eq!(mode_of(&path), FILE_MODE, "a rewrite must tighten the mode");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_world_readable_directory_is_tightened() {
        let dir = scratch("loose-dir");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        create_dir_all(&dir).unwrap();
        assert_eq!(mode_of(&dir), DIR_MODE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restricting_a_missing_file_is_not_an_error() {
        let dir = scratch("missing");
        create_dir_all(&dir).unwrap();
        restrict_existing(&dir.join("never-existed")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
