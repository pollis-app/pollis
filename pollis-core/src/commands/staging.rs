//! Attachments the renderer holds as bytes, staged in memory instead of on
//! disk.
//! No temp file: a leaked `/tmp` copy kept the original filename, which is content.
//! Released on upload success, remove/unmount, `lock`/`logout`/wipe; bounded by the ceiling.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{Error, Result};

/// Ceiling on everything staged at once.
/// Total, not per-file. Past it a stage is refused rather than evicting a visible card.
pub const STAGED_MAX_TOTAL_BYTES: usize = 512 * 1024 * 1024;

/// What the renderer gets back for a staged attachment: an opaque handle and
/// the size the backend actually received.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedAttachment {
    pub id: String,
    pub size_bytes: usize,
}

/// The staged bytes, keyed by id.
/// `Zeroizing` so the buffer is wiped when the last handle goes.
type Registry = Mutex<HashMap<String, Arc<Zeroizing<Vec<u8>>>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<String, Arc<Zeroizing<Vec<u8>>>>> {
    // Poison: the map has no invariant a panic could half-break.
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
/// An upload that fails leaves the bytes staged, so a retry works.
pub fn peek_staged(id: &str) -> Option<Arc<Zeroizing<Vec<u8>>>> {
    lock().get(id).cloned()
}

/// Release one staged attachment. Returns whether there was one.
pub fn discard_staged(id: &str) -> bool {
    lock().remove(id).is_some()
}

/// Release everything staged. Called from `lock`, `logout` and `wipe_local_data`.
pub fn discard_all_staged() {
    lock().clear();
}

/// Bytes currently staged.
pub fn staged_total_bytes() -> usize {
    lock().values().map(|b| b.len()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the tests: the registry is process-global.
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
        assert!(peek_staged(&staged.id).is_some());

        assert!(discard_staged(&staged.id));
        assert!(peek_staged(&staged.id).is_none());
        assert!(!discard_staged(&staged.id), "a second discard invents nothing");
    }

    #[test]
    fn an_unknown_handle_resolves_to_nothing() {
        let _serial = exclusive();
        assert!(peek_staged("01JNOTAREALSTAGEDID000000").is_none());
    }

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

    #[test]
    fn ending_a_session_releases_everything() {
        let _serial = exclusive();
        stage_attachment(vec![0u8; 64]).expect("stage");
        stage_attachment(vec![0u8; 64]).expect("stage");
        assert_eq!(staged_total_bytes(), 128);

        discard_all_staged();

        assert_eq!(staged_total_bytes(), 0);
    }

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
