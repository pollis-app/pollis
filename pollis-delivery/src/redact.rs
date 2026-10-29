//! Redaction helpers so logs don't record user PII (#345): log sinks are a
//! broader-retention tier than the access-controlled `users` table.

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
