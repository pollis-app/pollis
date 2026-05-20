use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // webrtc-sys adds -ObjC but doesn't always propagate to final link;
    // this ensures ObjC categories (e.g. NSString+StdString) are kept.
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-arg=-ObjC");

    stage_capture_helper();

    tauri_build::build()
}

/// Build the per-OS screen-capture helper and stage it both at the Tauri
/// `externalBin` sidecar path (`src-tauri/binaries/<helper>-<triple>`,
/// used in production bundles) and at the dev location
/// (`target/<profile>/<helper>`, used by `pnpm dev` where
/// `locate_capture_helper` in pollis-core finds it). Skipped on Windows
/// (capture is in-process via WGC). Skipped when the sidecar is already
/// present so CI's pre-built artifact (Linux: built on ubuntu-24.04 to
/// match PipeWire 1.0; the app job on 22.04 would fail to compile
/// libspa) is used as-is.
fn stage_capture_helper() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let helper = match target_os.as_str() {
        "linux" => "pollis-capture-linux",
        "macos" => "pollis-capture-macos",
        _ => return,
    };
    let target = std::env::var("TARGET").expect("TARGET set by cargo");
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".into());

    // Absolute paths — build.rs runs with cwd = src-tauri/, but we
    // invoke the inner cargo with cwd = workspace root, and CARGO_TARGET_DIR
    // is resolved relative to whoever consumes it. Using absolutes here
    // sidesteps all of that.
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo"),
    );
    let workspace_root = manifest_dir.parent().expect("workspace root").to_path_buf();
    let helper_src = workspace_root.join(helper);
    println!(
        "cargo:rerun-if-changed={}",
        helper_src.join("Cargo.toml").display()
    );
    println!("cargo:rerun-if-changed={}", helper_src.join("src").display());
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("pollis-capture-proto/src").display()
    );

    let sidecar_dir = PathBuf::from("binaries");
    let sidecar = sidecar_dir.join(format!("{helper}-{target}"));
    // The dev location pollis-core's `locate_capture_helper` searches in
    // debug builds. Keeping it in sync with the helper crate's source
    // means `pnpm dev` always launches the freshly-built helper without
    // anybody running an extra `cargo build` by hand.
    let dev_copy = workspace_root.join("target").join(&profile).join(helper);

    // Re-run if either staged copy disappears (someone removed it, or a
    // fresh checkout). Without this, cargo caches build.rs's last result
    // and skips it even when the targets are gone.
    println!("cargo:rerun-if-changed={}", sidecar.display());
    println!("cargo:rerun-if-changed={}", dev_copy.display());

    if sidecar.exists() && dev_copy.exists() {
        return;
    }
    std::fs::create_dir_all(&sidecar_dir).expect("create src-tauri/binaries");

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());

    if target == "universal-apple-darwin" {
        // No rustc target for universal; lipo the two arch slices. This
        // path is release-only (`tauri build --target universal-apple-darwin`);
        // dev never sees it, so skip the dev-copy step.
        let arm = build_helper(&cargo, &workspace_root, helper, "aarch64-apple-darwin", &profile);
        let intel = build_helper(&cargo, &workspace_root, helper, "x86_64-apple-darwin", &profile);
        let status = Command::new("lipo")
            .arg("-create")
            .arg(&arm)
            .arg(&intel)
            .arg("-output")
            .arg(&sidecar)
            .status()
            .expect("invoke lipo");
        assert!(status.success(), "lipo failed for {helper}");
    } else {
        let built = build_helper(&cargo, &workspace_root, helper, &target, &profile);
        std::fs::copy(&built, &sidecar).unwrap_or_else(|e| {
            panic!("copy {} -> {}: {e}", built.display(), sidecar.display())
        });
        // Mirror the binary at the dev-friendly path so `pnpm dev` /
        // `tauri dev` (debug builds) actually launch this freshly-built
        // helper rather than a stale one or the older release sidecar.
        if let Some(parent) = dev_copy.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::copy(&built, &dev_copy).unwrap_or_else(|e| {
            panic!("copy {} -> {}: {e}", built.display(), dev_copy.display())
        });
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perm = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&sidecar, perm.clone()).expect("chmod sidecar");
        if dev_copy.exists() {
            std::fs::set_permissions(&dev_copy, perm).expect("chmod dev copy");
        }
    }
}

/// Build the helper at the parent's profile so dev = debug and
/// `tauri build` / CI = release. Avoids the asymmetry where the
/// helper was always-release but the dev parent searched debug first.
///
/// Critical: a nested `cargo build` cannot reuse the parent's target dir
/// — the parent holds `target/.cargo-lock` for the duration of build.rs
/// and the inner cargo would deadlock waiting for it. We give the helper
/// its own `target/capture-helper/` dir, which has its own lock domain.
fn build_helper(
    cargo: &str,
    workspace_root: &Path,
    helper: &str,
    target: &str,
    profile: &str,
) -> PathBuf {
    let inner_target_dir = workspace_root.join("target").join("capture-helper");
    let mut cmd = Command::new(cargo);
    cmd.current_dir(workspace_root)
        .env("CARGO_TARGET_DIR", &inner_target_dir)
        .args(["build", "-p", helper, "--target", target]);
    if profile == "release" {
        cmd.arg("--release");
    }
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("invoke cargo for {helper} ({target}): {e}"));
    assert!(status.success(), "cargo build failed for {helper} ({target})");
    inner_target_dir.join(target).join(profile).join(helper)
}
