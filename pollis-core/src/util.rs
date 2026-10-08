//! Tiny crate-wide primitives that have no better home.
//!
//! Everything here was, until #875, copy-pasted into whichever module needed it
//! — `pin.rs`, `mls/ds_client.rs` and `net/overlay.rs` each carried their own
//! wall-clock helper under three different names. Identical copies are cheap
//! until one of them is edited.

/// Current wall-clock time, whole seconds since the Unix epoch.
///
/// A clock behind the epoch (only reachable if the system clock is wildly wrong)
/// reads as `0` rather than panicking: every caller uses this for TTLs and
/// rate-limit stamps, where "a very old timestamp" degrades gracefully and a
/// panic does not.
///
/// Returns `u64` because a Unix second is not negative. Callers whose downstream
/// arithmetic is signed — request-signature skew, which must not wrap when a
/// client's clock is ahead — cast at the boundary.
/// The name this device registers under: the OS and the form factor, e.g.
/// "macOS desktop" or "Android device".
///
/// It used to be `"{hostname} ({os})"`. A hostname is chosen by whoever set the
/// machine up and routinely carries a person's name or — on cloud and corporate
/// machines (`ip-10-0-0-12`) — an IP address, and it was stored in the DS's
/// `user_device` table where operators can read it. The device list needs to
/// tell a user's devices apart, not identify the machine, and the creation date
/// beside each entry does the rest.
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
/// Devices registered by builds before this change are named
/// `"{hostname} ({os})"`. The hostname must not be copied anywhere new, so a
/// name of that shape is reduced to the OS label [`device_label`] would give
/// that OS; any other name (a current OS label, a name chosen at device link)
/// passes through.
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

/// Base64-encode `bytes` with the standard, padded alphabet — the encoding every
/// `pollis-api` wire field uses.
///
/// The decoding half is `commands::ds_reads::decode_b64`, which stays there
/// because it takes a field name for its error message and every caller is
/// already reading a DS response. This half had four identical copies (`vault`,
/// `pinned_messages`, `messages::read_state`, and a closure in `mls::delivery`)
/// for the same reason `now_unix` had three.
pub fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Mask an email's local part, keeping the domain: `alice@example.com` →
/// `***@example.com`. Anything without a domain → `***`.
///
/// The client's half of the redaction the Delivery Service already does
/// (`pollis_delivery::redact::mask_email`). Same shape on purpose — a log line
/// should read the same whichever side wrote it — and duplicated rather than
/// shared because `pollis-delivery` is the server and nothing in the client
/// depends on it.
///
/// The client's logs are not a smaller problem than the server's. `pollis-tui`
/// redirects fd 2 into `pollis-tui.log`, which lands in the OS temp directory
/// when `POLLIS_DATA_DIR` is unset — so `email=alice@example.com` on a signup
/// line was the account's real address, in the clear, in a file every other
/// local user could read.
pub fn mask_email(email: &str) -> String {
    match email.trim().rsplit_once('@') {
        Some((_local, domain)) if !domain.is_empty() => format!("***@{domain}"),
        _ => "***".to_string(),
    }
}

/// Longest string [`is_safe_id`] accepts. A ULID — what the Delivery Service
/// mints for `user_id` and `device_id` — is 26 characters; the bound leaves room
/// for any id shape the DS has ever handed out while keeping an id far too short
/// to be anything but one filename.
pub const MAX_SAFE_ID_LEN: usize = 64;

/// Is `id` safe to use as a single path component?
///
/// Accepts `[A-Za-z0-9_-]{1,64}` and nothing else — no separators (`/`, `\`),
/// no `.` (so neither `.` nor `..`), no whitespace, no NUL, nothing the OS could
/// interpret. The point is where an id ends up: `media-cache/<user_id>/` and
/// `pollis_<user_id>.db` are built with `Path::join`, and `Path::join` with an
/// ABSOLUTE right-hand side replaces the base while `..` walks out of it. Every
/// id a server hands the client (`verify-otp`'s `user_id` first of all) goes
/// through this before it is persisted or joined onto anything, so a malicious
/// or compromised Delivery Service cannot name `/home/alice` as an account and
/// have the media-cache wipe empty it.
///
/// Deliberately a shape check rather than a strict ULID parse: the property the
/// filesystem needs is "one plain component", and tightening to 26 Crockford
/// characters would strand any account whose id predates the ULID scheme.
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

    /// The same cases `pollis_delivery::redact`'s own tests pin, so the two
    /// copies cannot drift into disagreeing about what a masked address is.
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
