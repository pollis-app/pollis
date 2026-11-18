//! The development grants file is compiled out of release builds (#2004).
//!
//! Builds `examples/release_probe.rs`, which calls `dev_grants::load`, twice: in
//! the dev profile it must compile (so the probe is valid), and in the release
//! profile it must fail because the module does not exist. Removing the
//! `#[cfg(debug_assertions)]` gate makes the release check succeed and this
//! test fail.

use std::path::PathBuf;
use std::process::Command;

fn check(release: bool) -> (bool, String) {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // A separate target dir: the outer `cargo test` holds the lock on the
    // workspace one.
    let target_dir = manifest_dir.join("../target/release-probe");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(&manifest_dir)
        .args([
            "check",
            "--quiet",
            "-p",
            "pollis-app-manifest",
            "--features",
            "release-probe",
            "--example",
            "release_probe",
            "--target-dir",
        ])
        .arg(&target_dir);
    if release {
        cmd.arg("--release");
    }
    let out = cmd.output().expect("run cargo check");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn dev_grants_file_is_compiled_out_of_release_builds() {
    let (debug_ok, debug_err) = check(false);
    assert!(
        debug_ok,
        "probe must compile in a debug build:\n{debug_err}"
    );

    let (release_ok, release_err) = check(true);
    assert!(
        !release_ok,
        "a release build can still load grants from a dev file"
    );
    assert!(
        release_err.contains("dev_grants"),
        "release build failed for another reason:\n{release_err}"
    );
}
