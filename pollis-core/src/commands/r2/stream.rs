//! An outbox upload (a conversation's or the Vault's) fed one chunk at a time (#2100).
//!
//! The outbox keeps an attachment's bytes inside the encrypted local database. Reading the whole
//! file back, encrypting it in memory and handing the ciphertext to the PUT held two copies of a
//! 100 MB file at once, which an iOS share extension cannot afford. Here the caller reads the
//! stored bytes [`CHUNK_SIZE`] at a time; each chunk is hashed and sealed (the same chunked
//! AES-256-GCM as [`super::encrypt_chunked`], so the object is byte-identical), the ciphertext
//! goes to a temporary file in the data dir, and the PUT streams that file. Peak memory is a
//! couple of chunks whatever the file size.
//!
//! The temporary file holds only ciphertext under a fresh per-file key, and is deleted when the
//! upload ends, succeeds or not ([`TempFile`]); a killed process's leftovers are swept by the
//! next upload ([`sweep_stale`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use super::{MediaUploadResult, UploadScope, CHUNK_SIZE};
use crate::error::{Error, Result};
use crate::state::AppState;

/// The temporary ciphertext files' directory, inside the data dir.
const TMP_DIR: &str = "upload-tmp";

/// An image at most this large is decoded for its blurhash and size, as the in-memory path
/// does. A larger one keeps what the sender supplied (the share extension measures with the
/// platform decoder): decoding it here would cost more memory than the whole upload.
pub(crate) const IMAGE_META_MAX_BYTES: u64 = 24 * 1024 * 1024;

/// How old a leftover temporary file must be before [`sweep_stale`] removes it. Far longer
/// than any upload in flight takes to write and send its file.
const STALE_SECS: u64 = 6 * 60 * 60;

/// Deletes its file on drop.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Remove temporary files a killed process left behind. Best-effort.
fn sweep_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs() > STALE_SECS);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The nonce of chunk `index`: the base nonce with the little-endian index XORed into its first
/// four bytes, exactly as [`super::encrypt_chunked`] derives it.
pub(super) fn chunk_nonce(base: &[u8; 12], index: u32) -> [u8; 12] {
    let mut nonce = *base;
    for (n, b) in nonce.iter_mut().zip(index.to_le_bytes()) {
        *n ^= b;
    }
    nonce
}

/// One upload in progress. [`StreamUpload::push`] takes the plaintext in order, every chunk but
/// the last exactly [`CHUNK_SIZE`] long; [`StreamUpload::finish`] sends it.
pub(crate) struct StreamUpload {
    cipher: Aes256Gcm,
    base_nonce: [u8; 12],
    key_version: i64,
    content_hash: String,
    r2_key: String,
    r2_url: String,
    content_type: String,
    hasher: Sha256,
    /// The plaintext of a small image, for its blurhash; `None` for anything else.
    image: Option<zeroize::Zeroizing<Vec<u8>>>,
    plain_len: u64,
    expected_len: u64,
    chunks: u32,
    /// Set once a short chunk has been pushed: nothing may follow it.
    ended: bool,
    file: tokio::fs::File,
    tmp: TempFile,
    cipher_len: u64,
    _slot: tokio::sync::SemaphorePermit<'static>,
}

impl StreamUpload {
    /// Start an upload of `expected_len` bytes sealed for `scope`. The ring key is resolved
    /// first, so an unreachable ring fails before any byte is read.
    pub(crate) async fn begin(
        state: &Arc<AppState>,
        scope: UploadScope,
        content_type: &str,
        expected_len: u64,
    ) -> Result<Self> {
        if expected_len > pollis_api::broker::R2_MEDIA_MAX_BYTES {
            return Err(Error::Other(anyhow::anyhow!(
                "attachment is {expected_len} bytes; the limit is {} bytes",
                pollis_api::broker::R2_MEDIA_MAX_BYTES
            )));
        }
        let slot = super::upload_slot().await;
        let (key_version, ring_key) = super::upload_ring_key(state, &scope).await?;
        let mut file_id = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut file_id);
        let content_hash = hex::encode(file_id);
        let r2_key = format!("media/{content_hash}.enc");
        let r2_url = format!("{}/{}", state.config.r2_endpoint.trim_end_matches('/'), r2_key);
        let (enc_key, base_nonce) = crate::commands::file_keys::derive_file_key(&ring_key, &file_id);
        drop(ring_key);
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&*enc_key));
        drop(enc_key);

        let dir = state.local_data_dir().join(TMP_DIR);
        crate::private_fs::create_dir_all(&dir)
            .map_err(|e| Error::Other(anyhow::anyhow!("create upload dir: {e}")))?;
        sweep_stale(&dir);
        let path = dir.join(format!("{content_hash}.enc"));
        let file = tokio::fs::File::create(&path)
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!("create upload file: {e}")))?;
        let image = (content_type.starts_with("image/") && expected_len <= IMAGE_META_MAX_BYTES)
            .then(|| zeroize::Zeroizing::new(Vec::with_capacity(expected_len as usize)));
        Ok(Self {
            cipher,
            base_nonce,
            key_version,
            content_hash,
            r2_key,
            r2_url,
            content_type: content_type.to_string(),
            hasher: Sha256::new(),
            image,
            plain_len: 0,
            expected_len,
            chunks: 0,
            ended: false,
            file,
            tmp: TempFile(path),
            cipher_len: 0,
            _slot: slot,
        })
    }

    /// Hash and seal the next chunk of plaintext, and append its ciphertext to the file.
    pub(crate) async fn push(&mut self, chunk: &[u8]) -> Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        if self.ended || chunk.len() > CHUNK_SIZE {
            return Err(Error::Other(anyhow::anyhow!("upload chunks out of shape")));
        }
        if chunk.len() < CHUNK_SIZE {
            self.ended = true;
        }
        self.plain_len += chunk.len() as u64;
        if self.plain_len > self.expected_len {
            return Err(Error::Other(anyhow::anyhow!("attachment grew while it was read")));
        }
        self.hasher.update(chunk);
        if let Some(buf) = self.image.as_mut() {
            buf.extend_from_slice(chunk);
        }
        let nonce = chunk_nonce(&self.base_nonce, self.chunks);
        let ct = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), chunk)
            .map_err(|_| Error::Other(anyhow::anyhow!("attachment encryption failed")))?;
        self.chunks += 1;
        self.cipher_len += ct.len() as u64;
        self.file
            .write_all(&ct)
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!("write upload file: {e}")))?;
        Ok(())
    }

    /// Send the sealed file and register the object. `filename` is what the envelope names it.
    pub(crate) async fn finish(
        mut self,
        state: &Arc<AppState>,
        filename: String,
    ) -> Result<MediaUploadResult> {
        if self.plain_len != self.expected_len {
            return Err(Error::Other(anyhow::anyhow!(
                "attachment is {} bytes, not the {} stored",
                self.plain_len,
                self.expected_len
            )));
        }
        self.file
            .flush()
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!("flush upload file: {e}")))?;
        self.file
            .sync_all()
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!("sync upload file: {e}")))?;
        let sha256 = hex::encode(self.hasher.finalize_reset());
        let (blurhash, width, height) = match self.image.take() {
            Some(bytes) => {
                let ct = self.content_type.clone();
                match tokio::task::spawn_blocking(move || super::compute_image_meta(&bytes)).await {
                    Ok(Ok((bh, w, h))) => (Some(bh), Some(w), Some(h)),
                    Ok(Err(e)) => {
                        crate::diag!("[upload_media] image meta failed ({ct}): {e}");
                        (None, None, None)
                    }
                    Err(_) => (None, None, None),
                }
            }
            None => (None, None, None),
        };

        #[cfg(feature = "test-harness")]
        let stubbed = crate::commands::messages::test_hooks::uploads_stubbed();
        #[cfg(not(feature = "test-harness"))]
        let stubbed = false;
        if !stubbed {
            let put_url =
                super::presign_r2_with_length(state, "put", &self.r2_key, Some(self.cipher_len))
                    .await?;
            let file = tokio::fs::File::open(&self.tmp.0)
                .await
                .map_err(|e| Error::Other(anyhow::anyhow!("open upload file: {e}")))?;
            let overlay = state.overlay_handle();
            super::r2_put_body(
                overlay.as_deref(),
                &put_url,
                file_body(file),
                self.cipher_len,
                "application/octet-stream",
            )
            .await?;
            let body = pollis_api::messages::AttachmentRegisterBody::ObjectOnly {
                content_hash: self.content_hash.clone(),
                r2_key: self.r2_key.clone(),
            };
            crate::commands::mls::ds_post_ok(state, &body).await?;
        }

        Ok(MediaUploadResult {
            key: self.r2_key.clone(),
            url: self.r2_url.clone(),
            filename,
            content_type: self.content_type.clone(),
            size_bytes: self.plain_len as usize,
            content_hash: self.content_hash.clone(),
            key_version: self.key_version,
            sha256,
            blurhash,
            width,
            height,
            poster: None,
        })
    }
}

/// A request body that reads `file` 64 KiB at a time.
fn file_body(file: tokio::fs::File) -> reqwest::Body {
    use tokio::io::AsyncReadExt;
    const READ: usize = 64 * 1024;
    let stream = futures_util::stream::try_unfold(file, |mut file| async move {
        let mut buf = vec![0u8; READ];
        let n = file.read(&mut buf).await?;
        if n == 0 {
            return Ok::<_, std::io::Error>(None);
        }
        buf.truncate(n);
        Ok(Some((bytes::Bytes::from(buf), file)))
    });
    reqwest::Body::wrap_stream(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The streamed object is the in-memory one, chunk for chunk: a reader that decrypts with
    /// `decrypt_chunked` cannot tell which path sealed it.
    #[test]
    fn a_streamed_object_matches_the_in_memory_one() {
        let key = [7u8; 32];
        let base = [9u8; 12];
        let data: Vec<u8> = (0..(CHUNK_SIZE * 2 + 1234)).map(|i| (i % 251) as u8).collect();
        let whole = super::super::encrypt_chunked(&data, &key, &base);
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
        let mut streamed = Vec::new();
        for (i, chunk) in data.chunks(CHUNK_SIZE).enumerate() {
            let nonce = chunk_nonce(&base, i as u32);
            streamed.extend(cipher.encrypt(Nonce::from_slice(&nonce), chunk).unwrap());
        }
        assert_eq!(whole, streamed);
        assert_eq!(super::super::decrypt_chunked(&streamed, &key, &base).unwrap(), data);
    }

    #[test]
    fn stale_temporary_files_are_swept_and_fresh_ones_kept() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join("fresh.enc");
        let old = dir.path().join("old.enc");
        std::fs::write(&fresh, b"x").unwrap();
        std::fs::write(&old, b"x").unwrap();
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(STALE_SECS + 60);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        sweep_stale(dir.path());
        assert!(fresh.exists());
        assert!(!old.exists());
    }

    #[test]
    fn a_temp_file_is_deleted_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.enc");
        std::fs::write(&path, b"x").unwrap();
        drop(TempFile(path.clone()));
        assert!(!path.exists());
    }
}
