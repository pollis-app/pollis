//! `get_diagnostics` (#1305): the on-device log ([`crate::diag`]) with a short
//! header, for the user to copy out and send. Read-only, local-only — nothing
//! here touches the network.

/// The report the "Copy diagnostics" button puts on the clipboard. The host
/// passes its own version (the desktop bundle / mobile store build), which is
/// not the core crate's.
pub fn get_diagnostics(app_version: Option<String>) -> String {
    let log = crate::diag::read_log();
    format!(
        "Pollis diagnostics\napp: {}\ncore: {}\nos: {}/{}\ngenerated: {}\n----\n{}",
        app_version.as_deref().unwrap_or("unknown"),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
        if log.is_empty() { "(no log recorded on this device)\n".to_string() } else { log },
    )
}
