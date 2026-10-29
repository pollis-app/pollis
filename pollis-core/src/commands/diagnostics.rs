//! `get_diagnostics` (#1305): the on-device log ([`crate::diag`]) with a short
//! header, for the user to copy out and send. Local-only.

/// The "Copy diagnostics" report. `app_version` is the host's build version, not
/// the core crate's.
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
