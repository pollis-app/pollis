//! Attachments the renderer holds as bytes, staged in memory instead of on
//! disk.
//!
//! # Why this module exists
//!
//! A pasted or dragged-in file arrives in the webview as a `File` object with
//! no filesystem path, and the upload path wants one — so the renderer used to
//! write the raw source bytes to the OS temp directory as
//! `pollis-<timestamp>-<original filename>` and hand `upload_media` that path.
//! Nothing ever deleted it. Not the upload, not `removeAttachment` (which
//! revoked the preview's blob URL and dropped the array entry), not the send,
//! not app exit. Every file a user had ever pasted was still sitting in `/tmp`
//! (or `%TEMP%`) in the clear, under its original name — while the R2 key
//! format deliberately drops filenames because a filename is content
//! ("budget-2026.pdf").
//!
//! Deleting the file on every exit path would be a rule to follow, and the rule
//! was already there to follow. This removes the file instead: bytes come over
//! the IPC into this registry, `upload_media_staged` reads them from here, and
//! nothing ever names a path. A leak needs a file to leak.
//!
//! # What the bytes cost
//!
//! They are plaintext user content held in RAM, so:
//!
//! * an upload **releases** its entry the moment it succeeds
//!   ([`discard_staged`]), and holds it if the upload fails, so a flaky network
//!   is a retry rather than a lost paste;
//! * the renderer discards on remove and on unmount;
//! * `lock`, `logout` and `wipe_local_data` call [`discard_all_staged`], so
//!   staged bytes never outlive the session that produced them;
//! * process exit frees everything, with nothing left behind to find;
//! * and [`STAGED_MAX_TOTAL_BYTES`] bounds the damage if every one of those
//!   were somehow missed — a new stage is REFUSED rather than evicting an
//!   attachment the user is still composing with.
//!
//! The renderer already held the whole file in the webview's heap (it called
//! `file.arrayBuffer()` to write the temp file), so this moves a copy rather
//! than adding one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{Error, Result};

/// Ceiling on everything staged at once.
///
/// Not a per-file limit: one 400 MB video and four 100 MB ones cost the same
/// RSS, and the number that matters is the total. Composing past this is
/// refused with an error the renderer surfaces, which is the honest outcome —
/// silently dropping the oldest staged attachment would remove a card the user
/// can still see.
pub const STAGED_MAX_TOTAL_BYTES: usize = 512 * 1024 * 1024;

/// What the renderer gets back for a staged attachment: an opaque handle and
/// the size the backend actually received.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedAttachment {
    /// Opaque id. The renderer's only reference to the bytes; there is no path
    /// to hand around, which is the point of the module.
    pub id: String,
    pub size_bytes: usize,
}

/// The staged bytes, keyed by id.
///
/// `Arc` so an upload can read them without copying while the registry keeps
/// ownership until the upload succeeds. `Zeroizing` so the buffer is wiped when
/// the last handle goes, rather than left in freed heap.
type Registry = Mutex<HashMap<String, Arc<Zeroizing<Vec<u8>>>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<String, Arc<Zeroizing<Vec<u8>>>>> {
    // A poisoned lock means a panic while holding it. The map is a plain
    // HashMap with no invariant a panic could half-break, and refusing every
    // future paste for the life of the process is worse than carrying on.
    registry().lock().unwrap_or_else(|e| e.into_inner())
}

/// Take custody of `bytes` and return the handle that stands for them.
pub fn stage_attachment(bytes: Vec<u8>) -> Result<StagedAttachment> {
    let size_bytes = bytes.len();
    let mut map = lock();
    let staged: usize = map.values().map(|b| b.len()).sum();
    if staged.saturating_add(size_bytes) > STAGED_MAX_TOTAL_BYTES {
        return Err(Error::Other(anyhow::anyhow!(
            "cannot stage another {size_bytes} bytes: {staged} are already staged and the \
             ceiling is {STAGED_MAX_TOTAL_BYTES}"
        )));
    }
    let id = ulid::Ulid::new().to_string();
    map.insert(id.clone(), Arc::new(Zeroizing::new(bytes)));
    Ok(StagedAttachment { id, size_bytes })
}

/// Read a staged attachment without releasing it.
///
/// The upload path uses this rather than a take, so an upload that fails leaves
/// the bytes staged and the user's paste survives a retry. [`discard_staged`]
/// is what releases them, once the upload has actually succeeded.
pub fn peek_staged(id: &str) -> Option<Arc<Zeroizing<Vec<u8>>>> {
    lock().get(id).cloned()
}

/// Release one staged attachment. Returns whether there was one.
pub fn discard_staged(id: &str) -> bool {
    lock().remove(id).is_some()
}

/// Release everything staged. Called from `lock`, `logout` and
/// `wipe_local_data`: staged bytes are the user's plaintext, and a session
/// ending is exactly when they stop being anyone's business.
pub fn discard_all_staged() {
    lock().clear();
}

/// Bytes currently staged. For the ceiling's tests, and for a caller that wants
/// to report the cost.
pub fn staged_total_bytes() -> usize {
    lock().values().map(|b| b.len()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the tests: the registry is process-global, and the ceiling
    /// test asserts against a total.
    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        static SERIAL: Mutex<()> = Mutex::new(());
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        discard_all_staged();
        guard
    }

    #[test]
    fn staged_bytes_come_back_under_their_handle_and_are_released_once() {
        let _serial = exclusive();
        let staged = stage_attachment(b"a pasted screenshot".to_vec()).expect("stage");
        assert_eq!(staged.size_bytes, 19);

        let peeked = peek_staged(&staged.id).expect("still staged");
        assert_eq!(&***peeked, b"a pasted screenshot");
        // A peek must NOT release: an upload that fails has to be retryable.
        assert!(peek_staged(&staged.id).is_some());

        assert!(discard_staged(&staged.id));
        assert!(peek_staged(&staged.id).is_none());
        assert!(!discard_staged(&staged.id), "a second discard invents nothing");
    }

    /// An unknown id is `None`, not a panic and not somebody else's bytes.
    #[test]
    fn an_unknown_handle_resolves_to_nothing() {
        let _serial = exclusive();
        assert!(peek_staged("01JNOTAREALSTAGEDID000000").is_none());
    }

    /// Ids do not collide, so one attachment can never be uploaded in place of
    /// another.
    #[test]
    fn handles_are_distinct() {
        let _serial = exclusive();
        let a = stage_attachment(vec![1, 2, 3]).expect("stage");
        let b = stage_attachment(vec![4, 5, 6]).expect("stage");
        assert_ne!(a.id, b.id);
        assert_eq!(&***peek_staged(&a.id).unwrap(), &[1, 2, 3]);
        assert_eq!(&***peek_staged(&b.id).unwrap(), &[4, 5, 6]);
        discard_all_staged();
    }

    /// The session boundary. Whatever the renderer did or forgot to do, a lock
    /// or a logout leaves nothing staged.
    #[test]
    fn ending_a_session_releases_everything() {
        let _serial = exclusive();
        stage_attachment(vec![0u8; 64]).expect("stage");
        stage_attachment(vec![0u8; 64]).expect("stage");
        assert_eq!(staged_total_bytes(), 128);

        discard_all_staged();

        assert_eq!(staged_total_bytes(), 0);
    }

    /// The ceiling refuses rather than evicting — an attachment the user can
    /// still see on screen must not silently stop existing.
    #[test]
    fn the_ceiling_refuses_instead_of_evicting() {
        let _serial = exclusive();
        let big = stage_attachment(vec![0u8; 1024]).expect("stage");

        let refused = stage_attachment(vec![0u8; STAGED_MAX_TOTAL_BYTES]);
        assert!(refused.is_err(), "the ceiling did not hold");
        assert!(
            peek_staged(&big.id).is_some(),
            "a refused stage evicted an attachment that was already queued"
        );
        discard_all_staged();
    }
}
