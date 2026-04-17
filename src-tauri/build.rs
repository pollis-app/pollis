fn main() {
    // Fix Objective-C category loading for WebRTC on macOS
    // webrtc-sys adds -ObjC but it doesn't always propagate to final link
    // This ensures ObjC categories (like NSString+StdString) are included
    #[cfg(target_os = "macos")]
    {
        // Add ObjC linker flag to ensure categories are loaded
        println!("cargo:rustc-link-arg=-ObjC");
    }

    // MAS builds compile out `tauri-plugin-updater` and `tauri-plugin-process`,
    // so any capability file that references their permissions (`updater:*`,
    // `process:*`) would fail ACL validation. The default capability file at
    // `capabilities/default.json` is kept lean (no updater/process perms) and
    // the updater-specific perms live in `capabilities/updater/*.json`, which
    // is only included when the `updater` feature is on.
    #[cfg(feature = "mas")]
    let attrs = tauri_build::Attributes::new()
        .capabilities_path_pattern("./capabilities/default.json");

    #[cfg(all(feature = "updater", not(feature = "mas")))]
    let attrs = tauri_build::Attributes::new()
        .capabilities_path_pattern("./capabilities/**/*.json");

    #[cfg(not(any(feature = "mas", feature = "updater")))]
    let attrs = tauri_build::Attributes::new()
        .capabilities_path_pattern("./capabilities/default.json");

    if let Err(error) = tauri_build::try_build(attrs) {
        let error = format!("{error:#}");
        println!("cargo:warning={}", error);
        std::process::exit(1);
    }
}
