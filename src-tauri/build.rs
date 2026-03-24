fn main() {
    // Fix Objective-C category loading for WebRTC on macOS
    // webrtc-sys adds -ObjC but it doesn't always propagate to final link
    // This ensures ObjC categories (like NSString+StdString) are included
    #[cfg(target_os = "macos")]
    {
        // Add ObjC linker flag to ensure categories are loaded
        println!("cargo:rustc-link-arg=-ObjC");
    }

    tauri_build::build()
}
