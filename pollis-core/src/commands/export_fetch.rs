//! The opt-in second step of the export (#856): download the attachments the
//! archive could not satisfy from the local cache.
//!
//! Deliberately a separate module and a separate command. `export.rs` is
//! strictly device-local and its own test scans it for network helpers; this
//! module is the one place the export feature is allowed to touch R2, and only
//! when the user pressed a second, separately-worded button *after* the archive
//! was written. The default path never comes here.
//!
//! What it fetches is exactly `ExportSummary::attachments_missing` — a list the
//! renderer hands back — so every entry is treated as untrusted: `files_dir`
//! must be absolute and each `file` must be a single path component, or the
//! whole call is refused before any byte moves. The bytes themselves come
//! through [`crate::commands::r2::download_media`], which decrypts and verifies
//! each object against its content hash, so a substituted or corrupted object
//! is a per-file failure, never a file on disk.

use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use crate::commands::export::MissingAttachment;
use crate::error::{Error, Result};
use crate::state::AppState;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FetchSummary {
    /// Files now present and verified under `files_dir` — including any that
    /// were already there from an earlier run, so the call is idempotent.
    pub fetched: usize,
    pub failed: Vec<FetchFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FetchFailure {
    pub content_hash: String,
    pub file: String,
    pub error: String,
}

fn is_single_component(file: &str) -> bool {
    !file.is_empty()
        && file != "."
        && file != ".."
        && !file.contains('/')
        && !file.contains('\\')
        && !file.contains('\0')
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// The fetch loop with the download injected, so the tests can drive it with
/// a fake and the command with the real R2 path.
pub(crate) async fn fetch_with<F, Fut>(
    files_dir: &Path,
    attachments: &[MissingAttachment],
    fetch: F,
) -> Result<FetchSummary>
where
    F: Fn(&MissingAttachment) -> Fut,
    Fut: Future<Output = Result<Vec<u8>>>,
{
    if !files_dir.is_absolute() {
        return Err(Error::Other(anyhow::anyhow!("files_dir must be absolute")));
    }
    if let Some(bad) = attachments.iter().find(|a| !is_single_component(&a.file)) {
        return Err(Error::Other(anyhow::anyhow!(
            "refusing attachment file name {:?}: must be a single path component",
            bad.file
        )));
    }
    std::fs::create_dir_all(files_dir)
        .map_err(|e| Error::Other(anyhow::anyhow!("could not create {}: {e}", files_dir.display())))?;

    let mut fetched = 0usize;
    let mut failed = Vec::new();
    for att in attachments {
        let target = files_dir.join(&att.file);
        // Already there and correct — an earlier run, or the cache step. Don't
        // download what the disk already holds.
        if let Ok(existing) = std::fs::read(&target) {
            if sha256_hex(&existing) == att.content_hash {
                fetched += 1;
                continue;
            }
        }
        match fetch(att).await {
            Ok(bytes) => {
                // `download_media` verifies; a fake might not. Never vouch for
                // bytes on disk that do not match their name.
                if sha256_hex(&bytes) != att.content_hash {
                    failed.push(FetchFailure {
                        content_hash: att.content_hash.clone(),
                        file: att.file.clone(),
                        error: "content hash mismatch".to_string(),
                    });
                    continue;
                }
                match std::fs::write(&target, bytes) {
                    Ok(()) => fetched += 1,
                    Err(e) => failed.push(FetchFailure {
                        content_hash: att.content_hash.clone(),
                        file: att.file.clone(),
                        error: e.to_string(),
                    }),
                }
            }
            Err(e) => failed.push(FetchFailure {
                content_hash: att.content_hash.clone(),
                file: att.file.clone(),
                error: e.to_string(),
            }),
        }
    }
    Ok(FetchSummary { fetched, failed })
}

/// Download the attachments an export reported missing into its files
/// directory. **This is the one network step of the export, and it only runs
/// on an explicit second request.** Sequential by design: one presigned GET
/// at a time, so a large archive cannot fan out into a burst against the DS.
pub async fn fetch_export_attachments(
    files_dir: String,
    attachments: Vec<MissingAttachment>,
    state: &Arc<AppState>,
) -> Result<FetchSummary> {
    fetch_with(Path::new(&files_dir), &attachments, |att| {
        crate::commands::r2::download_media(att.storage_key.clone(), att.content_hash.clone(), state)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn missing(name: &str, bytes: &[u8]) -> MissingAttachment {
        MissingAttachment {
            content_hash: sha256_hex(bytes),
            storage_key: format!("r2/{name}"),
            content_type: None,
            file: name.to_string(),
        }
    }

    #[tokio::test]
    async fn fetches_verifies_and_writes_each_missing_attachment() {
        let dir = tempfile::tempdir().unwrap();
        let files = dir.path().join("archive-files");
        let atts = vec![missing("a.png", b"aaa"), missing("b.png", b"bbb")];
        let summary = fetch_with(&files, &atts, |att| {
            let bytes = if att.file == "a.png" { b"aaa".to_vec() } else { b"bbb".to_vec() };
            async move { Ok(bytes) }
        })
        .await
        .unwrap();
        assert_eq!(summary.fetched, 2);
        assert!(summary.failed.is_empty());
        assert_eq!(std::fs::read(files.join("a.png")).unwrap(), b"aaa");
        assert_eq!(std::fs::read(files.join("b.png")).unwrap(), b"bbb");
    }

    #[tokio::test]
    async fn a_failed_download_is_reported_and_the_rest_still_land() {
        let dir = tempfile::tempdir().unwrap();
        let atts = vec![missing("bad.png", b"x"), missing("good.png", b"y")];
        let summary = fetch_with(dir.path(), &atts, |att| {
            let ok = att.file == "good.png";
            async move {
                if ok {
                    Ok(b"y".to_vec())
                } else {
                    Err(Error::Other(anyhow::anyhow!("network down")))
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(summary.fetched, 1);
        assert_eq!(summary.failed.len(), 1);
        assert_eq!(summary.failed[0].file, "bad.png");
        assert!(summary.failed[0].error.contains("network down"));
        assert!(!dir.path().join("bad.png").exists());
    }

    #[tokio::test]
    async fn substituted_bytes_never_reach_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        let atts = vec![missing("a.png", b"meant")];
        let summary = fetch_with(dir.path(), &atts, |_| async { Ok(b"substituted".to_vec()) })
            .await
            .unwrap();
        assert_eq!(summary.fetched, 0);
        assert_eq!(summary.failed[0].error, "content hash mismatch");
        assert!(!dir.path().join("a.png").exists());
    }

    #[tokio::test]
    async fn a_file_already_present_is_not_downloaded_again() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), b"aaa").unwrap();
        let atts = vec![missing("a.png", b"aaa")];
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let summary = fetch_with(dir.path(), &atts, |_| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Ok(b"aaa".to_vec()) }
        })
        .await
        .unwrap();
        assert_eq!(summary.fetched, 1);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_stale_file_with_the_wrong_bytes_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), b"old").unwrap();
        let atts = vec![missing("a.png", b"new")];
        let summary = fetch_with(dir.path(), &atts, |_| async { Ok(b"new".to_vec()) })
            .await
            .unwrap();
        assert_eq!(summary.fetched, 1);
        assert_eq!(std::fs::read(dir.path().join("a.png")).unwrap(), b"new");
    }

    /// The list comes from the renderer. A crafted `file` must not be able to
    /// write outside `files_dir`, and nothing is fetched before that is known.
    #[tokio::test]
    async fn a_file_name_that_is_not_one_component_refuses_the_whole_call() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["../escape.png", "sub/dir.png", "..", "", "a\\b", "c\0d"] {
            let mut att = missing("ok.png", b"ok");
            att.file = bad.to_string();
            let atts = vec![missing("first.png", b"first"), att];
            let calls = std::sync::atomic::AtomicUsize::new(0);
            let err = fetch_with(dir.path(), &atts, |_| {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Ok(b"first".to_vec()) }
            })
            .await
            .unwrap_err();
            assert!(err.to_string().contains("single path component"), "{bad:?}: {err}");
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0, "{bad:?} fetched before validation");
            assert!(!dir.path().join("first.png").exists());
        }
        assert!(!dir.path().parent().unwrap().join("escape.png").exists());
    }

    #[tokio::test]
    async fn a_relative_files_dir_is_refused() {
        let err = fetch_with(Path::new("relative"), &[], |_| async { Ok(Vec::new()) })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("absolute"));
    }
}
