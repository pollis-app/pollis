//! The soak corpus: seeded random files, one exact duplicate, real media.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rand::{RngCore, SeedableRng};
use sha2::{Digest, Sha256};

use crate::proofs::{distinct, Needle, Scanner};

const WINDOW: usize = 64;
const WINDOWS_PER_FILE: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Random,
    Media,
    /// Byte-identical to corpus file `.0`.
    Duplicate(usize),
}

#[derive(Clone, Debug)]
pub struct SoakFile {
    pub label: String,
    pub kind: Kind,
    pub path: PathBuf,
    /// Holds `token`, so the name itself is a needle.
    pub filename: String,
    pub token: String,
    pub content_type: String,
    pub size: u64,
    pub sha256: [u8; 32],
    /// Plaintext samples (offset, bytes): the needles.
    pub samples: Vec<(String, Vec<u8>)>,
}

impl SoakFile {
    pub fn sha_hex(&self) -> String {
        hex::encode(self.sha256)
    }
}

fn content_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        Some("flac") => "audio/flac",
        _ => "application/octet-stream",
    }
}

fn fingerprint(path: &Path, size: u64) -> ([u8; 32], Vec<(String, Vec<u8>)>) {
    let mut f = std::fs::File::open(path).expect("open corpus file");
    let mut hasher = Sha256::new();
    let mut whole = Vec::new();
    let mut buf = vec![0u8; 8 * 1024 * 1024];
    loop {
        let n = f.read(&mut buf).expect("read corpus file");
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        if size <= 4 * WINDOW as u64 {
            whole.extend_from_slice(&buf[..n]);
        }
    }
    let sha: [u8; 32] = hasher.finalize().into();

    let read_at = |off: u64, len: usize| -> Vec<u8> {
        use std::io::Seek;
        let mut f = std::fs::File::open(path).expect("reopen corpus file");
        f.seek(std::io::SeekFrom::Start(off)).expect("seek");
        let mut out = vec![0u8; len];
        f.read_exact(&mut out).expect("read sample");
        out
    };

    let mut samples = Vec::new();
    if size < WINDOW as u64 {
        if !whole.is_empty() && distinct(&whole) >= 4 {
            samples.push(("whole file".to_string(), whole));
        }
        return (sha, samples);
    }
    let prefix = read_at(0, WINDOW);
    if distinct(&prefix) >= 8 {
        samples.push(("prefix[0..64]".to_string(), prefix));
    }
    let suffix = read_at(size - WINDOW as u64, WINDOW);
    if distinct(&suffix) >= 8 {
        samples.push((format!("suffix[{}..]", size - WINDOW as u64), suffix));
    }
    // Low-entropy media walks forward to the first distinctive window.
    for i in 1..=WINDOWS_PER_FILE {
        let mut off = size * i as u64 / (WINDOWS_PER_FILE as u64 + 1);
        for _ in 0..64 {
            if off + WINDOW as u64 > size {
                break;
            }
            let w = read_at(off, WINDOW);
            if distinct(&w) >= 20 {
                samples.push((format!("window@{off}"), w));
                break;
            }
            off += 4096;
        }
    }
    (sha, samples)
}

fn token(rng: &mut impl RngCore) -> String {
    let mut t = [0u8; 8];
    rng.fill_bytes(&mut t);
    hex::encode(t)
}

fn write_random(path: &Path, size: u64, seed: u64) {
    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).expect("create corpus file"));
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    let mut left = size;
    while left > 0 {
        let n = left.min(buf.len() as u64) as usize;
        rng.fill_bytes(&mut buf[..n]);
        f.write_all(&buf[..n]).expect("write corpus file");
        left -= n as u64;
    }
    f.flush().expect("flush corpus file");
}

pub fn build(dir: &Path, sizes: &[u64], media_dir: Option<&Path>, seed: u64) -> Vec<SoakFile> {
    std::fs::create_dir_all(dir).expect("create corpus dir");
    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    let mut files = Vec::new();

    for (i, &size) in sizes.iter().enumerate() {
        let tok = token(&mut rng);
        let filename = format!("soak-{tok}-r{i:02}.bin");
        let path = dir.join(&filename);
        write_random(&path, size, seed.wrapping_add(i as u64 + 1));
        let (sha256, samples) = fingerprint(&path, size);
        files.push(SoakFile {
            label: format!("r{i:02}"),
            kind: Kind::Random,
            path,
            filename,
            token: tok,
            content_type: "application/octet-stream".to_string(),
            size,
            sha256,
            samples,
        });
    }

    // Identical uploads must share nothing (#1382 removed convergent encryption).
    if let Some(orig) = files.iter().position(|f| f.kind == Kind::Random) {
        let src = files[orig].clone();
        let tok = token(&mut rng);
        let filename = format!("soak-{tok}-dup-of-{}.bin", src.label);
        let path = dir.join(&filename);
        std::fs::copy(&src.path, &path).expect("copy duplicate");
        files.push(SoakFile {
            label: format!("d-{}", src.label),
            kind: Kind::Duplicate(orig),
            path,
            filename,
            token: tok,
            ..src
        });
    }

    if let Some(mdir) = media_dir {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(mdir)
            .unwrap_or_else(|e| panic!("POLLIS_SOAK_MEDIA_DIR {}: {e}", mdir.display()))
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        entries.sort();
        for (j, src) in entries.into_iter().enumerate() {
            let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("bin").to_string();
            let tok = token(&mut rng);
            let filename = format!("soak-{tok}-m{j:02}.{ext}");
            let path = dir.join(&filename);
            std::fs::copy(&src, &path).expect("copy media file");
            let size = std::fs::metadata(&path).expect("stat media").len();
            if size == 0 {
                continue;
            }
            let (sha256, samples) = fingerprint(&path, size);
            files.push(SoakFile {
                label: format!("m{j:02}"),
                kind: Kind::Media,
                content_type: content_type_for(&path).to_string(),
                path,
                filename,
                token: tok,
                size,
                sha256,
                samples,
            });
        }
    }
    files
}

/// Needles: sample windows, name tokens and plaintext SHA-256s (hex and raw).
pub fn add_needles(scanner: &mut Scanner, files: &[SoakFile]) {
    for (i, f) in files.iter().enumerate() {
        for (what, bytes) in &f.samples {
            scanner.add(bytes.clone(), Needle { file: Some(i), what: format!("{} plaintext {what}", f.label), secret: false });
        }
        scanner.add(f.token.clone().into_bytes(), Needle { file: Some(i), what: format!("{} filename", f.label), secret: false });
        scanner.add(f.sha_hex().into_bytes(), Needle { file: Some(i), what: format!("{} plaintext sha256 (hex)", f.label), secret: false });
        scanner.add(f.sha256.to_vec(), Needle { file: Some(i), what: format!("{} plaintext sha256 (raw)", f.label), secret: false });
    }
}
