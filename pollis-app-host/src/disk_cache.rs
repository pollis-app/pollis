//! The compiled-component cache on disk: a performance cache only.
//!
//! An entry is `<dir>/<hex App hash>.cwasm`:
//!
//! ```text
//! MAGIC (16) | App hash (32) | SHA-256 of the component bytes (32)
//!            | engine fingerprint (8) | SHA-256 of the payload (32) | payload
//! ```
//!
//! where the payload is `Component::serialize` output and the fingerprint is
//! a hash of wasmtime's own compatibility hash (its version and every setting
//! that affects compiled code). An entry is used only when all of these match
//! the App being loaded and the running engine and the payload is intact;
//! anything else is reported [`Lookup::Rejected`] and the caller compiles
//! afresh and overwrites it. A missing directory or entry is simply a miss.
//!
//! Loading compiled code trusts its bytes (wasmtime's `deserialize` is
//! `unsafe` for that reason). The checks above catch a stale, mismatched or
//! corrupted entry; they are not a defence against someone who can write the
//! runtime's data directory, who can equally replace the runtime itself. The
//! directory is created owner-only.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use wasmtime::component::Component;
use wasmtime::Engine;

const MAGIC: &[u8; 16] = b"pollis-cwasm-v1\0";
const HEADER: usize = 16 + 32 + 32 + 8 + 32;

pub enum Lookup {
    Hit(Component),
    Absent,
    Rejected,
}

fn path(dir: &Path, app_hash: &[u8; 32]) -> PathBuf {
    let name: String = app_hash.iter().map(|b| format!("{b:02x}")).collect();
    dir.join(format!("{name}.cwasm"))
}

/// The running engine's compatibility fingerprint. `DefaultHasher::new()` is
/// a fixed-key SipHash, the same across processes of the same build.
pub fn fingerprint(engine: &Engine) -> [u8; 8] {
    let mut h = std::hash::DefaultHasher::new();
    engine.precompile_compatibility_hash().hash(&mut h);
    h.finish().to_le_bytes()
}

pub fn load(engine: &Engine, dir: &Path, app_hash: &[u8; 32], digest: &[u8; 32]) -> Lookup {
    let Ok(bytes) = std::fs::read(path(dir, app_hash)) else {
        return Lookup::Absent;
    };
    if bytes.len() <= HEADER
        || &bytes[..16] != MAGIC
        || &bytes[16..48] != app_hash
        || &bytes[48..80] != digest
        || bytes[80..88] != fingerprint(engine)
    {
        return Lookup::Rejected;
    }
    let payload = &bytes[HEADER..];
    if Sha256::digest(payload).as_slice() != &bytes[88..120] {
        return Lookup::Rejected;
    }
    // SAFETY: the payload is a `Component::serialize` output this host wrote
    // for exactly this App and this engine (key, fingerprint and checksum all
    // match). wasmtime additionally refuses an incompatible module.
    match unsafe { Component::deserialize(engine, payload) } {
        Ok(c) => Lookup::Hit(c),
        Err(_) => Lookup::Rejected,
    }
}

/// Write an entry, atomically (temp file + rename). Failure only costs the
/// next cold start a compile, so it is ignored.
pub fn store(engine: &Engine, dir: &Path, app_hash: &[u8; 32], digest: &[u8; 32], component: &Component) {
    let Ok(payload) = component.serialize() else {
        return;
    };
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(app_hash);
    out.extend_from_slice(digest);
    out.extend_from_slice(&fingerprint(engine));
    out.extend_from_slice(&Sha256::digest(&payload));
    out.extend_from_slice(&payload);
    let _ = write(dir, &path(dir, app_hash), &out);
}

fn write(dir: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
