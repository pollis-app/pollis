//! Server side of the invite-link token format (#847). Mirrors
//! `pollis-core/src/commands/groups/invite_token.rs` (the crates share no code),
//! so **a change to the token format must land in both files.** The DS never
//! mints a token; it only parses, re-hashes and compares.

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Split a presented token into `(selector, secret)`.
///
/// Returns `None` for anything malformed. Every caller must map `None` onto the
/// exact same rejection it uses for a non-matching token — see
/// `apply_redeem_invite_link`.
pub fn parse(token: &str) -> Option<(String, String)> {
    let trimmed = token.trim();
    let (selector, secret) = trimmed.split_once('.')?;
    if selector.is_empty() || secret.is_empty() {
        return None;
    }
    // Not a secret-dependent branch: it reveals only that the input was malformed.
    let ok = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    if !ok(selector) || !ok(secret) {
        return None;
    }
    Some((selector.to_string(), secret.to_string()))
}

/// Lowercase hex `sha256` of the secret half. Hashes the base64 TEXT, matching
/// `pollis-core`'s `hash_secret`.
pub fn hash_secret(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hex::encode(hasher.finalize())
}

/// Constant-time equality over the decoded 32 bytes of two hex hashes. A
/// malformed stored value never matches (fails closed); the length check leaks
/// nothing, since the format fixes both lengths.
pub fn hash_eq(a: &str, b: &str) -> bool {
    let (Ok(a), Ok(b)) = (hex::decode(a), hex::decode(b)) else {
        return false;
    };
    if a.len() != 32 || b.len() != 32 {
        return false;
    }
    a.ct_eq(&b).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    // This vector is the contract with pollis-core's `hash_secret`.
    #[test]
    fn hash_secret_matches_the_pollis_core_vector() {
        assert_eq!(
            hash_secret("test-secret"),
            "9caf06bb4436cdbfa20af9121a626bc1093c4f54b31c0fa937957856135345b6"
        );
    }

    #[test]
    fn hash_eq_is_true_only_for_identical_hashes() {
        let a = hash_secret("alpha");
        let b = hash_secret("beta");
        assert!(hash_eq(&a, &a));
        assert!(!hash_eq(&a, &b));
    }

    #[test]
    fn hash_eq_fails_closed_on_malformed_stored_values() {
        let good = hash_secret("alpha");
        assert!(!hash_eq(&good, "zzzz"));
        assert!(!hash_eq(&good, "abcd"));
        assert!(!hash_eq(&good, ""));
        assert!(!hash_eq("", ""));
    }

    #[test]
    fn parse_rejects_malformed_tokens() {
        assert!(parse("").is_none());
        assert!(parse("nodot").is_none());
        assert!(parse(".onlysecret").is_none());
        assert!(parse("onlyselector.").is_none());
        assert!(parse("ab*c.def").is_none());
        assert!(parse("abc.de f").is_none());
        assert!(parse("sel.sec").is_some());
    }
}
