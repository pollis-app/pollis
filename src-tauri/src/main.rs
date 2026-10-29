// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Disable the WebKitGTK DMABuf renderer before any window exists; EGL hard-aborts on some Linux systems.
    #[cfg(target_os = "linux")]
    {
        // Opt in to GPU compositing (WebGL screenshare render) with POLLIS_ENABLE_COMPOSITING=1.
        if std::env::var("POLLIS_ENABLE_COMPOSITING").is_err() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
            // DMABUF alone isn't enough on some drivers (NVIDIA, VMs).
            std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        }
    }

    pollis_lib::run();
}
