//! Tiny crate-wide primitives that have no better home.

/// The name this device registers under: the OS and the form factor, e.g.
/// "macOS desktop". Never the hostname, which can name a person or embed an
/// IP and is readable by DS operators.
pub fn device_label() -> String {
    let os = std::env::consts::OS;
    let pretty = match os {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        "ios" => "iOS",
        "android" => "Android",
        other => other,
    };
    let kind = match os {
        "ios" | "android" => "device",
        _ => "desktop",
    };
    format!("{pretty} {kind}")
}

/// A device name safe to copy into the account's security log: a legacy
/// `"{hostname} ({os})"` name is reduced to the OS label [`device_label`] gives.
pub fn public_device_name(name: &str) -> String {
    const LEGACY: [(&str, &str); 5] = [
        ("(macos)", "macOS desktop"),
        ("(windows)", "Windows desktop"),
        ("(linux)", "Linux desktop"),
        ("(ios)", "iOS device"),
        ("(android)", "Android device"),
    ];
    for (suffix, label) in LEGACY {
        if name.ends_with(suffix) && name.len() > suffix.len() {
            return label.to_string();
        }
    }
    name.to_string()
}

/// Current wall-clock time, whole seconds since the Unix epoch. A clock behind
/// the epoch reads as `0` rather than panicking.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Base64-encode `bytes` with the standard, padded alphabet used by every
/// `pollis-api` wire field (decode: `commands::ds_reads::decode_b64`).
pub fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Mask an email's local part, keeping the domain: `alice@example.com` →
/// `***@example.com`. Anything without a domain → `***`.
///
/// Mirrors `pollis_delivery::redact::mask_email` so log lines read the same on
/// both sides; client logs can land in a world-readable temp dir.
pub fn mask_email(email: &str) -> String {
    match email.trim().rsplit_once('@') {
        Some((_local, domain)) if !domain.is_empty() => format!("***@{domain}"),
        _ => "***".to_string(),
    }
}

/// Longest string [`is_safe_id`] accepts (a ULID is 26).
pub const MAX_SAFE_ID_LEN: usize = 64;

/// Is `id` safe to use as a single path component?
///
/// Accepts `[A-Za-z0-9_-]{1,64}` only. Server-issued ids are joined onto paths
/// (`media-cache/<user_id>/`, `pollis_<user_id>.db`), where an absolute id or
/// `..` would escape the base, so a malicious DS could make the media-cache
/// wipe empty an arbitrary directory. A shape check, not a ULID parse, so
/// pre-ULID ids still work.
pub fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_SAFE_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the DS actually mints, plus the shapes older or test accounts use.
    #[test]
    fn plain_ids_are_safe() {
        assert!(is_safe_id("01JCACHEUSERLIFECYCLE0000"));
        assert!(is_safe_id(&ulid::Ulid::new().to_string()));
        assert!(is_safe_id("wipe-user-a"));
        assert!(is_safe_id("u1"));
        assert!(is_safe_id("_anon"));
        assert!(is_safe_id(&"a".repeat(MAX_SAFE_ID_LEN)));
    }

    /// Everything `Path::join` could turn into a different directory.
    #[test]
    fn anything_that_could_be_a_path_is_not() {
        assert!(!is_safe_id(""));
        assert!(!is_safe_id("/home/alice"));
        assert!(!is_safe_id("C:\\Users\\alice"));
        assert!(!is_safe_id(".."));
        assert!(!is_safe_id("."));
        assert!(!is_safe_id("../../.."));
        assert!(!is_safe_id("user/../.."));
        assert!(!is_safe_id("user.db"));
        assert!(!is_safe_id("user id"));
        assert!(!is_safe_id("user\0id"));
        assert!(!is_safe_id("üser"));
        assert!(!is_safe_id(&"a".repeat(MAX_SAFE_ID_LEN + 1)));
    }

    /// The same cases `pollis_delivery::redact`'s tests pin, so the copies cannot drift.
    #[test]
    fn masking_keeps_the_domain_and_nothing_else() {
        assert_eq!(mask_email("alice@example.com"), "***@example.com");
        assert_eq!(mask_email("  Bob.Smith@Mail.CO  "), "***@Mail.CO");
        assert_eq!(mask_email("not-an-email"), "***");
        assert_eq!(mask_email(""), "***");
        assert_eq!(mask_email("trailing@"), "***");
    }

    /// The local part is what identifies the person, so it must not survive in
    /// any form — not truncated, not initialled.
    #[test]
    fn the_local_part_never_survives() {
        let masked = mask_email("dangerously.identifying@example.com");
        assert!(!masked.contains("dangerously"));
        assert!(!masked.contains('d') || masked.starts_with("***@"));
    }

    /// The registered device name carries the OS and form factor only — never
    /// the machine's hostname, which can name a person or embed an IP.
    #[test]
    fn device_label_never_includes_the_hostname() {
        let label = device_label();
        let host = gethostname_for_test();
        if !host.is_empty() {
            assert!(!label.contains(&host), "{label:?} contains hostname {host:?}");
        }
        assert!(label.ends_with(" desktop") || label.ends_with(" device"), "{label:?}");
    }

    fn gethostname_for_test() -> String {
        std::process::Command::new("hostname")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }

    #[test]
    fn legacy_hostname_names_reduce_to_the_os_label() {
        assert_eq!(public_device_name("ip-10-0-0-5 (linux)"), "Linux desktop");
        assert_eq!(public_device_name("dans-macbook (macos)"), "macOS desktop");
        assert_eq!(public_device_name("macOS desktop"), "macOS desktop");
        assert_eq!(public_device_name("Pollis on Windows"), "Pollis on Windows");
    }
}
