// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Disable the WebKitGTK DMABuf renderer before any window is created.
    // The transparent window requires EGL compositing, which hard-aborts on
    // Linux systems without full EGL support. Setting this env var skips that
    // path entirely, regardless of how the binary was launched.
    #[cfg(target_os = "linux")]
    {
        // spike/tauri-revival: the native screenshare render leg paints frames
        // into a WebGL canvas, which wants the GPU compositing pipeline these
        // two vars disable. The disable was added for a *transparent* window
        // (EGL hard-abort on NVIDIA/VM EGL); pollis's window is opaque, so on
        // GPUs with working EGL we want compositing ON. Opt in with
        // POLLIS_ENABLE_COMPOSITING=1; default stays the conservative path.
        if std::env::var("POLLIS_ENABLE_COMPOSITING").is_err() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
            // DMABUF alone isn't enough on some drivers (NVIDIA proprietary,
            // certain VM GPU configs) where EGL compositing still aborts.
            std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        }
    }

    pollis_lib::run();
}
