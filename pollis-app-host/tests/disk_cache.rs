//! The compiled-component cache on disk (#2004): a cold host loads what an
//! earlier one compiled, and never trusts an entry that does not match.

mod common;

use std::sync::Arc;
use std::time::Instant;

use common::*;
use pollis_app_host::*;

fn host_at(dir: &std::path::Path) -> AppHost {
    AppHost::new(HostConfig {
        compiled_cache_dir: Some(dir.to_path_buf()),
        ..HostConfig::default()
    })
    .unwrap()
}

fn entry(dir: &std::path::Path, hash: &AppHash) -> std::path::PathBuf {
    let name: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    dir.join(format!("{name}.cwasm"))
}

#[test]
fn a_cold_host_loads_what_an_earlier_one_compiled() {
    let dir = tempfile::tempdir().unwrap();
    let first = host_at(dir.path());
    first.prepare(APP_HASH, &component()).unwrap();
    assert_eq!(first.stats().compiled, 1);
    assert!(entry(dir.path(), &APP_HASH).is_file());

    let second = host_at(dir.path());
    let r = run_on(&second, Arc::new(Fake::default()), "inst-a", &manifest(), &admin(), on_message("now"));
    r.ok();
    assert_eq!(second.stats(), HostStats { compiled: 0, loaded_from_disk: 1, rejected_from_disk: 0 });
}

#[test]
fn a_corrupt_or_mismatched_entry_is_recompiled_never_loaded() {
    let dir = tempfile::tempdir().unwrap();
    host_at(dir.path()).prepare(APP_HASH, &component()).unwrap();
    let path = entry(dir.path(), &APP_HASH);
    let good = std::fs::read(&path).unwrap();

    // A flipped byte in the compiled code.
    let mut bad = good.clone();
    let last = bad.len() - 1;
    bad[last] ^= 0xff;
    std::fs::write(&path, &bad).unwrap();
    let host = host_at(dir.path());
    host.prepare(APP_HASH, &component()).unwrap();
    assert_eq!(host.stats(), HostStats { compiled: 1, loaded_from_disk: 0, rejected_from_disk: 1 });
    assert_eq!(std::fs::read(&path).unwrap(), good, "the entry is rewritten");

    // Another App's entry under this App's name.
    let other: AppHash = [8u8; 32];
    std::fs::copy(&path, entry(dir.path(), &other)).unwrap();
    let host = host_at(dir.path());
    // Same component bytes, different App hash: the key inside does not match.
    host.prepare(other, &component()).unwrap();
    assert_eq!(host.stats().rejected_from_disk, 1);

    // This App's entry, but the component bytes changed.
    let host = host_at(dir.path());
    let err = host.prepare(APP_HASH, &std_component());
    assert!(err.is_ok(), "{err:?}");
    assert_eq!(host.stats(), HostStats { compiled: 1, loaded_from_disk: 0, rejected_from_disk: 1 });

    // Garbage and truncation.
    for junk in [&b"not a cache entry"[..], &good[..40]] {
        std::fs::write(&path, junk).unwrap();
        let host = host_at(dir.path());
        host.prepare(APP_HASH, &component()).unwrap();
        assert_eq!(host.stats().rejected_from_disk, 1);
    }
}

#[test]
fn no_cache_directory_is_fine() {
    let host = host();
    host.prepare(APP_HASH, &component()).unwrap();
    assert_eq!(host.stats(), HostStats { compiled: 1, loaded_from_disk: 0, rejected_from_disk: 0 });
}

/// Cold versus cached for a real component. Ignored by default; run with
///   POLLIS_APP_BENCH_COMPONENT=/path/to/app.wasm cargo test -p pollis-app-host --release \
///     --test disk_cache -- --ignored --nocapture
#[test]
#[ignore]
fn measure_cold_and_cached_compile() {
    let Ok(path) = std::env::var("POLLIS_APP_BENCH_COMPONENT") else {
        return;
    };
    let bytes = std::fs::read(&path).unwrap();
    let hash: AppHash = [3u8; 32];
    let time = |parallel: bool, dir: Option<&std::path::Path>| {
        let host = AppHost::new(HostConfig {
            compiled_cache_dir: dir.map(|d| d.to_path_buf()),
            parallel_compilation: parallel,
            ..HostConfig::default()
        })
        .unwrap();
        let start = Instant::now();
        // Imports may not match this host's world; the compile is what is timed.
        let _ = host.prepare(hash, &bytes);
        (start.elapsed(), host.stats())
    };
    let dir = tempfile::tempdir().unwrap();
    let single = time(false, None);
    let parallel = time(true, None);
    let cold = time(true, Some(dir.path()));
    let cached = time(true, Some(dir.path()));
    eprintln!(
        "[compile] {} bytes: single-threaded {:?}, parallel {:?}, cold+write {:?}, cached {:?} ({:?})",
        bytes.len(),
        single.0,
        parallel.0,
        cold.0,
        cached.0,
        cached.1
    );
}
