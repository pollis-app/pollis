use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // webrtc-sys's -ObjC doesn't always reach the final link; ObjC categories
    // (e.g. NSString+StdString) need it.
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-arg=-ObjC");

    // Headless builds (`native-shell` off) skip the capture helper: it pulls
    // PipeWire/libspa, which a headless CI box need not have.
    if std::env::var_os("CARGO_FEATURE_NATIVE_SHELL").is_some() {
        stage_capture_helper();
    } else {
        stage_placeholder_sidecar();
    }

    tauri_build::build()
}

/// Build the per-OS screen-capture helper and stage it at the `externalBin`
/// sidecar path (`binaries/<helper>-<triple>`, bundles) and at
/// `target/<profile>/<helper>` (where `locate_capture_helper` finds it in
/// dev). Not on Windows (capture is in-process via WGC).
fn stage_capture_helper() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let helper = match target_os.as_str() {
        "linux" => "pollis-capture-linux",
        "macos" => "pollis-capture-macos",
        _ => return,
    };
    let target = std::env::var("TARGET").expect("TARGET set by cargo");
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".into());

    // Absolute paths: build.rs runs in src-tauri/ but the inner cargo runs at
    // the workspace root.
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
    // Where pollis-core's `locate_capture_helper` looks in debug builds.
    let dev_copy = workspace_root.join("target").join(&profile).join(helper);

    // Without these, cargo skips build.rs even when a staged copy is gone.
    println!("cargo:rerun-if-changed={}", sidecar.display());
    println!("cargo:rerun-if-changed={}", dev_copy.display());

    // In CI the pre-staged sidecar is authoritative: the helper is built on
    // ubuntu-24.04 (PipeWire 1.0) and the app job on 22.04 can't compile
    // libspa. Locally an existing sidecar must NOT short-circuit: `tauri dev`
    // re-copies it over the dev path on every launch, so a stale one would
    // pin every dev run to an old helper.
    if sidecar.exists() && std::env::var_os("CI").is_some() {
        if !dev_copy.exists() {
            if let Some(parent) = dev_copy.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::copy(&sidecar, &dev_copy).unwrap_or_else(|e| {
                panic!("copy {} -> {}: {e}", sidecar.display(), dev_copy.display())
            });
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&dev_copy, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod dev copy");
            }
        }
        return;
    }
    std::fs::create_dir_all(&sidecar_dir).expect("create src-tauri/binaries");

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());

    if target == "universal-apple-darwin" {
        // No rustc target for universal; lipo the slices. Release-only, so no
        // dev copy.
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

/// Headless builds still need the `externalBin` sidecar to exist for
/// `tauri_build::build()`'s config check; stage a zero-byte placeholder that is
/// never executed or bundled.
fn stage_placeholder_sidecar() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let helper = match target_os.as_str() {
        "linux" => "pollis-capture-linux",
        "macos" => "pollis-capture-macos",
        _ => return,
    };
    let target = std::env::var("TARGET").expect("TARGET set by cargo");
    let sidecar_dir = PathBuf::from("binaries");
    std::fs::create_dir_all(&sidecar_dir).ok();
    let sidecar = sidecar_dir.join(format!("{helper}-{target}"));
    if !sidecar.exists() {
        std::fs::write(&sidecar, b"").expect("write placeholder sidecar");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o755));
        }
    }
}

/// Build the helper at the parent's profile, in its own
/// `target/capture-helper/`: reusing the parent's target dir deadlocks on
/// `target/.cargo-lock`, which the parent holds during build.rs.
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
