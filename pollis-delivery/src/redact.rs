//! Redaction helpers so application logs don't scatter user PII (#345).
//!
//! The server already holds emails in the `users` table, but that's access-
//! controlled storage; log sinks are a separate, often broader-retention tier.
//! Masking the local part keeps logs useful for debugging (the domain still
//! distinguishes a Resend/provider issue) without recording who the user is.

/// Mask an email's local part, keeping the domain: `alice@example.com` →
/// `***@example.com`. Anything without a domain → `***`.
pub fn mask_email(email: &str) -> String {
    match email.trim().rsplit_once('@') {
        Some((_local, domain)) if !domain.is_empty() => format!("***@{domain}"),
        _ => "***".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_local_part_keeps_domain() {
        assert_eq!(mask_email("alice@example.com"), "***@example.com");
        assert_eq!(mask_email("  Bob.Smith@Mail.CO  "), "***@Mail.CO");
    }

    #[test]
    fn no_domain_is_fully_masked() {
        assert_eq!(mask_email("not-an-email"), "***");
        assert_eq!(mask_email(""), "***");
        assert_eq!(mask_email("trailing@"), "***");
    }
}
