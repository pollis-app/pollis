#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "linux")]
    {
        // These disables avoid EGL hard-aborts (NVIDIA proprietary, VM GPUs)
        // but also turn off GPU compositing, which WebGL needs. Opt back in
        // with POLLIS_ENABLE_COMPOSITING=1.
        if std::env::var("POLLIS_ENABLE_COMPOSITING").is_err() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
            std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        }
    }

    pollis_lib::run();
}
