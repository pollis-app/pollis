//! Owner-only filesystem creation, in one place.
//!
//! Every file this app writes holds something that belongs to exactly one
//! person: `accounts.json` carries their real email address, `keystore.pks`
//! their wrapped identity keys, `pollis_<user>.db` their message history,
//! `overlay-guards.json` where they connect from, the media cache their
//! pictures. Created at the process umask — 0644 on a stock Linux or macOS
//! account — every one of those is readable by every other local user.
//!
//! The rule is one helper, not a `set_permissions` call sprinkled next to each
//! `File::create`: a write path that forgets is indistinguishable from one that
//! never had a mode, and there is no way to notice the omission later. So the
//! creation itself lives here and the mode is not a parameter — a caller cannot
//! ask for a world-readable file, because this module offers no way to say it.
//!
//! ## Platforms
//!
//! * **Unix** — files are created 0600 and directories 0700. The mode is passed
//!   to `open(2)`/`mkdir(2)` so the file is never briefly world-readable, and
//!   re-applied through the open handle (`fchmod`) so an *existing* file that
//!   predates this module — or one someone else pre-created — is tightened too,
//!   without a path-based `chmod` a symlink swap could redirect.
//! * **Windows** — deliberately nothing, and deliberately not silent. Files
//!   under `%APPDATA%` inherit the user profile directory's ACL, which grants
//!   the owning user, SYSTEM and Administrators and nobody else; that is
//!   already the property [`FILE_MODE`] buys on Unix. Writing our own ACLs
//!   would mean hand-rolling `SetNamedSecurityInfo` and a DACL, i.e. new code
//!   that can only be wrong in ways the inherited one cannot. The functions
//!   below compile to the plain `std::fs` operation there, and
//!   [`owner_only_is_enforced_by_mode`] states which of the two worlds you are
//!   in so a test can assert the difference rather than assume it.

use std::io;
use std::path::Path;

/// Mode for every file this app creates: owner read/write, nothing for group
/// or other.
#[cfg(unix)]
pub const FILE_MODE: u32 = 0o600;

/// Mode for every directory this app creates: owner only, including the
/// execute bit it needs to be traversable.
#[cfg(unix)]
pub const DIR_MODE: u32 = 0o700;

/// Whether this platform enforces owner-only access through an explicit mode
/// set by this module (Unix), or inherits it from the containing profile
/// directory's ACL (Windows).
///
/// Exists so the platform difference is a value tests can branch on instead of
/// a claim in a comment.
pub const fn owner_only_is_enforced_by_mode() -> bool {
    cfg!(unix)
}

/// Create `dir` and any missing parents, owner-only.
///
/// The mode is applied to the leaf directory even when it already exists, so a
/// data directory created by an older build (or by hand) is tightened on the
/// next run. Parents are left as `mkdir` made them: `dirs_path()` resolves
/// under `~/.local/share`, and this must not restyle the user's XDG tree on the
/// way down.
pub fn create_dir_all(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        use std::os::unix::fs::PermissionsExt;

        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(DIR_MODE)
            .create(dir)?;
        // `mkdir` is a no-op on a directory that already exists, so the mode
        // above only covers the ones this call just made.
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(DIR_MODE))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Create (or truncate) `path` for writing, owner-only, and hand back the open
/// handle.
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

/// [`write`], off the async runtime's worker thread.
///
/// The cache writes that use it run up to `MEDIA_CACHE_MAX_FILE_BYTES`
/// (100 MB), which is not something to do inline on a Tokio worker.
pub async fn write_async(path: &Path, contents: Vec<u8>) -> io::Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || write(&path, &contents))
        .await
        .map_err(|e| io::Error::other(format!("private write task: {e}")))?
}

/// Tighten a file something else created — the SQLite database and its `-wal`
/// / `-shm` sidecars, which the SQLite VFS opens and this module does not.
///
/// A missing file is not an error: the sidecars exist only while a connection
/// is open in WAL mode, and the caller should not have to know which.
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

/// Apply [`FILE_MODE`] through an already-open handle (`fchmod`), which no
/// path-based race can redirect.
///
/// Public because a few files cannot be created by this module — `pollis-tui`
/// opens its log in append mode so it can `dup2` fd 2 onto it — and the mode
/// passed to `open(2)` applies only when the open created the file. Tightening
/// through the handle covers the one left by an older build.
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
        // The platform predicate and the platform's actual behaviour, asserted
        // against each other: a `#[cfg(not(unix))]` branch that swallowed the
        // mode everywhere would still pass every assertion below on its own.
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

    /// The umask cannot loosen us, and neither can a file that was already
    /// there: the mode is re-applied through the open handle on every write.
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

    /// An existing loose directory is tightened too — a data dir created by a
    /// build that predates this module does not stay 0755 forever.
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
