//! The generated files agree with the versions the crate declares.

use std::collections::BTreeSet;

use sqlcipher_src::{
    source_dir, HEADER_FILE, LIBTOMCRYPT_INCLUDE_DIR, LIBTOMCRYPT_SOURCES, LIBTOMCRYPT_VERSION,
    SOURCE_FILE, SQLCIPHER_VERSION, SQLITE_VERSION,
};

fn read(file: &str) -> String {
    String::from_utf8_lossy(&std::fs::read(source_dir().join(file)).unwrap()).into_owned()
}

fn tomcrypt_header() -> String {
    format!("{LIBTOMCRYPT_INCLUDE_DIR}/tomcrypt.h")
}

#[test]
fn every_generated_file_is_present() {
    for file in [
        HEADER_FILE,
        SOURCE_FILE,
        &tomcrypt_header(),
        "LICENSE-sqlcipher",
        "LICENSE-libtomcrypt",
        "SHA256SUMS",
    ]
    .into_iter()
    .chain(LIBTOMCRYPT_SOURCES.iter().copied())
    {
        assert!(source_dir().join(file).is_file(), "{file} missing");
    }
}

#[test]
fn checksums_cover_every_generated_file() {
    let listed: Vec<String> = read("SHA256SUMS")
        .lines()
        .map(|l| l.split_once("  ").unwrap().1.to_owned())
        .collect();
    let present: Vec<String> = shipped_files()
        .into_iter()
        .filter(|name| name != "SHA256SUMS")
        .collect();
    assert_eq!(listed, present);
}

#[test]
fn sources_carry_the_declared_versions() {
    let header = read(HEADER_FILE);
    assert!(header
        .lines()
        .any(|l| l.starts_with("#define SQLITE_VERSION ")
            && l.contains(&format!("\"{SQLITE_VERSION}\""))));
    let sqlcipher = read(SOURCE_FILE);
    assert!(sqlcipher
        .lines()
        .any(|l| l.trim() == format!("#define CIPHER_VERSION_NUMBER {SQLCIPHER_VERSION}")));
    let tomcrypt = read(&tomcrypt_header());
    assert!(tomcrypt
        .lines()
        .any(|l| l.starts_with("#define SCRYPT")
            && l.contains(&format!("\"{LIBTOMCRYPT_VERSION}\""))));
}

#[test]
fn crate_version_encodes_the_release() {
    let mut parts = SQLCIPHER_VERSION
        .split('.')
        .map(|p| p.parse::<u64>().unwrap());
    let (major, minor, patch) = (
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
    );
    let (numbers, metadata) = env!("CARGO_PKG_VERSION").split_once('+').unwrap();
    assert!(
        numbers.starts_with(&format!("{}.{patch}.", major * 100 + minor)),
        "{numbers}"
    );
    assert_eq!(metadata, format!("sqlcipher-{SQLCIPHER_VERSION}-sqlite-{SQLITE_VERSION}-libtomcrypt-{LIBTOMCRYPT_VERSION}"));
}

/// Every file under [`source_dir`], by `/`-separated path relative to it, in sorted order.
fn shipped_files() -> Vec<String> {
    fn walk(dir: &std::path::Path, prefix: &str, files: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = format!("{prefix}{}", entry.file_name().into_string().unwrap());
            if entry.file_type().unwrap().is_dir() {
                walk(&entry.path(), &format!("{name}/"), files);
            } else {
                files.push(name);
            }
        }
    }
    let mut files = Vec::new();
    walk(source_dir(), "", &mut files);
    files.sort();
    files
}

fn has_extension(name: &str, extension: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        == Some(extension)
}

/// The `.c` files `file` includes by quote, resolved against its directory.
fn included_c_files(file: &str) -> Vec<String> {
    let dir = file.rsplit_once('/').map_or("", |(dir, _)| dir);
    read(file)
        .lines()
        .filter_map(|l| l.trim().strip_prefix("#include"))
        .filter_map(|l| l.trim().strip_prefix('"')?.strip_suffix('"'))
        .filter(|name| has_extension(name, "c"))
        .map(|name| {
            let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
            for part in name.split('/') {
                if part == ".." {
                    parts.pop().unwrap();
                } else {
                    parts.push(part);
                }
            }
            parts.join("/")
        })
        .collect()
}

#[test]
fn libtomcrypt_sources_are_every_file_but_the_tables() {
    let listed: BTreeSet<&str> = LIBTOMCRYPT_SOURCES.iter().copied().collect();
    assert_eq!(
        listed.len(),
        LIBTOMCRYPT_SOURCES.len(),
        "a source is listed twice"
    );
    let tables: BTreeSet<String> = listed.iter().flat_map(|s| included_c_files(s)).collect();
    assert!(!tables.is_empty());
    for table in &tables {
        assert!(
            !listed.contains(table.as_str()),
            "{table} is listed but included by another file"
        );
    }
    let shipped: BTreeSet<String> = shipped_files()
        .into_iter()
        .filter(|name| name.starts_with("libtomcrypt/") && has_extension(name, "c"))
        .collect();
    let expected: BTreeSet<String> = listed
        .iter()
        .map(|s| (*s).to_owned())
        .chain(tables)
        .collect();
    assert_eq!(shipped, expected);
}

#[test]
fn only_upstream_sources_ship() {
    for name in shipped_files() {
        let source = has_extension(&name, "c") || has_extension(&name, "h");
        assert!(
            source || name.starts_with("LICENSE-") || name == "SHA256SUMS",
            "{name} is not an upstream source"
        );
        // SQLCipher ships one C file, and everything else in C is libtomcrypt's tree.
        assert!(
            !has_extension(&name, "c") || name == SOURCE_FILE || name.starts_with("libtomcrypt/"),
            "{name} is not SQLCipher or libtomcrypt"
        );
    }
}

#[test]
fn sqlcipher_finalizer_is_skipped_on_wasm() {
    let sqlcipher = read(SOURCE_FILE);
    let lines: Vec<&str> = sqlcipher.lines().collect();
    let registration = lines
        .iter()
        .position(|l| l.contains("section(\".fini_array\")"))
        .expect("SQLCipher no longer registers a .fini_array finalizer");
    assert_eq!(lines[registration - 1], "#else");
    // The branch of the same unindented chain before that #else must take every wasm target.
    let branch = lines[..registration - 1]
        .iter()
        .rev()
        .find(|l| l.starts_with("#elif") || l.starts_with("#if"))
        .unwrap();
    assert_eq!(*branch, "#elif defined(__wasm__)");
}

#[test]
fn tomcrypt_headers_are_included_as_released() {
    // SQLCipher and libtomcrypt reach libtomcrypt's headers through the include path.
    assert!(read(SOURCE_FILE).contains("\n#include <tomcrypt.h>\n"));
    assert!(read(&tomcrypt_header()).contains("\n#include <tomcrypt_cfg.h>\n"));
    for name in shipped_files().iter().filter(|n| has_extension(n, "h")) {
        assert!(
            !read(name).contains("#include \"tomcrypt"),
            "{name} includes a tomcrypt header by quote, unlike the release"
        );
    }
}
