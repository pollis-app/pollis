// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Disable the WebKitGTK DMABuf renderer before any window is created.
    // The transparent window requires EGL compositing, which hard-aborts on
    // Linux systems without full EGL support. Setting this env var skips that
    // path entirely, regardless of how the binary was launched.
    #[cfg(target_os = "linux")]
    std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");

    pollis_lib::run();
}
