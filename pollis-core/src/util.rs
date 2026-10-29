//! Tiny crate-wide primitives that have no better home.

/// The name this device registers under: the OS and the form factor, e.g.
/// "macOS desktop" or "Android device".
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

/// A device name safe to copy into the account's security log.
///
/// Names of the legacy `"{hostname} ({os})"` shape are reduced to the OS label.
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

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Base64-encode `bytes` (standard, padded), the `pollis-api` wire encoding.
pub fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Mask an email's local part, keeping the domain: `alice@example.com` →
/// `***@example.com`. Anything without a domain → `***`.
pub fn mask_email(email: &str) -> String {
    match email.trim().rsplit_once('@') {
        Some((_local, domain)) if !domain.is_empty() => format!("***@{domain}"),
        _ => "***".to_string(),
    }
}

/// Longest string [`is_safe_id`] accepts; a ULID is 26.
pub const MAX_SAFE_ID_LEN: usize = 64;

/// Is `id` safe to use as a single path component?
///
/// Accepts `[A-Za-z0-9_-]{1,64}` only. Ids are `Path::join`ed into data paths, so an absolute or `..` id from a malicious DS could escape.
///
/// A shape check, not a ULID parse, so older id shapes still work.
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

    #[test]
    fn plain_ids_are_safe() {
        assert!(is_safe_id("01JCACHEUSERLIFECYCLE0000"));
        assert!(is_safe_id(&ulid::Ulid::new().to_string()));
        assert!(is_safe_id("wipe-user-a"));
        assert!(is_safe_id("u1"));
        assert!(is_safe_id("_anon"));
        assert!(is_safe_id(&"a".repeat(MAX_SAFE_ID_LEN)));
    }

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

    /// Same cases as `pollis_delivery::redact`'s tests, so the copies cannot drift.
    #[test]
    fn masking_keeps_the_domain_and_nothing_else() {
        assert_eq!(mask_email("alice@example.com"), "***@example.com");
        assert_eq!(mask_email("  Bob.Smith@Mail.CO  "), "***@Mail.CO");
        assert_eq!(mask_email("not-an-email"), "***");
        assert_eq!(mask_email(""), "***");
        assert_eq!(mask_email("trailing@"), "***");
    }

    #[test]
    fn the_local_part_never_survives() {
        let masked = mask_email("dangerously.identifying@example.com");
        assert!(!masked.contains("dangerously"));
        assert!(!masked.contains('d') || masked.starts_with("***@"));
    }

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
