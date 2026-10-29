use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // webrtc-sys's -ObjC doesn't always reach the final link; keeps ObjC categories.
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-arg=-ObjC");

    // Headless builds skip the helper: it pulls PipeWire/libspa a CI box need not have.
    if std::env::var_os("CARGO_FEATURE_NATIVE_SHELL").is_some() {
        stage_capture_helper();
    } else {
        stage_placeholder_sidecar();
    }

    tauri_build::build()
}

/// Build the capture helper and stage it at the `externalBin` sidecar path and the dev path. Not on Windows (WGC).
fn stage_capture_helper() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let helper = match target_os.as_str() {
        "linux" => "pollis-capture-linux",
        "macos" => "pollis-capture-macos",
        _ => return,
    };
    let target = std::env::var("TARGET").expect("TARGET set by cargo");
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".into());

    // Absolute paths: build.rs and the inner cargo run with different cwds.
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
    // pollis-core's `locate_capture_helper` searches here in debug builds.
    let dev_copy = workspace_root.join("target").join(&profile).join(helper);

    // Re-run if either staged copy disappears.
    println!("cargo:rerun-if-changed={}", sidecar.display());
    println!("cargo:rerun-if-changed={}", dev_copy.display());

    // In CI the pre-staged sidecar (built on ubuntu-24.04 for PipeWire 1.0) is authoritative. Locally always rebuild, or a stale gitignored sidecar pins every dev run (pre-#394).
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
        // No rustc target for universal; lipo the two slices. Release-only, so no dev copy.
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

/// Headless builds still need the `externalBin` sidecar to exist for `tauri_build`; stage a never-run placeholder.
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

/// Built at the parent's profile, in its own target dir: the parent holds `target/.cargo-lock`, so sharing it deadlocks.
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
