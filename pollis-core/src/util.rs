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

#[cfg(test)]
mod tests {
    use super::*;

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
}
