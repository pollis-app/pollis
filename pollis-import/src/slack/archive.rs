//! A Slack export as a set of named files, whether it is still a ZIP or was
//! unpacked into a folder. Admins download a ZIP; some unpack it first.
//!
//! Reads are bounded (a ZIP can claim any size) and entry names are only
//! ever looked up, never joined onto a path we write, so a crafted entry name
//! (`../x`) cannot place a file anywhere.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Largest JSON file read (one `users.json`, or one channel's day).
pub const JSON_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Largest image read: the emoji encoder's own input ceiling.
pub const IMAGE_MAX_BYTES: u64 = 8 * 1024 * 1024;

pub enum Archive {
    Dir(PathBuf),
    Zip {
        zip: zip::ZipArchive<File>,
        /// Relative name (prefix stripped, `/`-separated) → entry index.
        entries: BTreeMap<String, usize>,
        /// Where images inside the ZIP are copied so the upload can read them.
        cache: PathBuf,
    },
}

impl Archive {
    pub fn open(path: &Path) -> io::Result<Self> {
        if path.is_dir() {
            return Ok(Archive::Dir(path.to_path_buf()));
        }
        let file = File::open(path)?;
        let meta = file.metadata()?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        // Some tools wrap the export in one folder; root it where channels.json is.
        let mut names: Vec<(String, usize)> = Vec::new();
        for i in 0..zip.len() {
            let Ok(entry) = zip.by_index_raw(i) else {
                continue;
            };
            if entry.is_dir() {
                continue;
            }
            let Some(name) = entry.enclosed_name() else {
                continue;
            };
            let name = name.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            names.push((name, i));
        }
        let prefix = names
            .iter()
            .filter_map(|(n, _)| {
                n.strip_suffix("channels.json")
                    .filter(|p| p.is_empty() || p.ends_with('/'))
                    .map(str::to_string)
            })
            .min_by_key(|p| p.len())
            .unwrap_or_default();
        let entries = names
            .into_iter()
            .filter_map(|(n, i)| n.strip_prefix(&prefix).map(|rest| (rest.to_string(), i)))
            .collect();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf()).hash(&mut h);
        meta.len().hash(&mut h);
        meta.modified().ok().hash(&mut h);
        let cache = std::env::temp_dir().join("pollis-import").join(format!("slack-{:016x}", h.finish()));
        Ok(Archive::Zip { zip, entries, cache })
    }

    /// Every file, as a `/`-separated name relative to the export's root.
    /// A folder is listed two levels deep (`channels.json`, `general/2024-01-02.json`).
    pub fn names(&self) -> Vec<String> {
        match self {
            Archive::Dir(root) => {
                let mut out = Vec::new();
                list_dir(root, "", 2, &mut out);
                out.sort();
                out
            }
            Archive::Zip { entries, .. } => entries.keys().cloned().collect(),
        }
    }

    pub fn has(&self, name: &str) -> bool {
        match self {
            Archive::Dir(root) => root.join(name).is_file(),
            Archive::Zip { entries, .. } => entries.contains_key(name),
        }
    }

    /// The bytes of `name`, or `None` if there is no such file. Larger than
    /// `limit` is an error, not a truncation.
    pub fn read(&mut self, name: &str, limit: u64) -> io::Result<Option<Vec<u8>>> {
        let reader: Box<dyn Read + '_> = match self {
            Archive::Dir(root) => {
                let path = root.join(name);
                if !path.is_file() {
                    return Ok(None);
                }
                Box::new(File::open(path)?)
            }
            Archive::Zip { zip, entries, .. } => {
                let Some(&i) = entries.get(name) else {
                    return Ok(None);
                };
                Box::new(zip.by_index(i).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?)
            }
        };
        let mut buf = Vec::new();
        reader.take(limit + 1).read_to_end(&mut buf)?;
        if buf.len() as u64 > limit {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{name} is larger than {limit} bytes")));
        }
        Ok(Some(buf))
    }

    /// A path the upload can read `name` from: the file itself in a folder,
    /// a copy in the cache for a ZIP (named by a hash, never by the entry).
    pub fn local_file(&mut self, name: &str) -> io::Result<Option<PathBuf>> {
        if let Archive::Dir(root) = self {
            let path = root.join(name);
            return Ok(path.is_file().then_some(path));
        }
        let Some(bytes) = self.read(name, IMAGE_MAX_BYTES)? else {
            return Ok(None);
        };
        let Archive::Zip { cache, .. } = self else {
            unreachable!("a folder returned above");
        };
        let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("bin").to_ascii_lowercase();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        name.hash(&mut h);
        fs::create_dir_all(&*cache)?;
        let out = cache.join(format!("{:016x}.{ext}", h.finish()));
        fs::write(&out, bytes)?;
        Ok(Some(out))
    }
}

fn list_dir(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<String>) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = format!("{prefix}{name}");
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_file() {
            out.push(rel);
        } else if kind.is_dir() && depth > 1 {
            list_dir(&entry.path(), &format!("{rel}/"), depth - 1, out);
        }
    }
}
