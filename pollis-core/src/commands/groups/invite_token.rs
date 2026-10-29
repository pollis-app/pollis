//! #847 — invite-link token format.
//!
//! A token is two random halves joined by a dot:
//!
//! ```text
//!     <selector>.<secret>
//!      ^16 chars  ^43 chars      (base64url, unpadded)
//!      96 bits    256 bits
//! ```
//!
//! The server stores the selector (the indexed lookup handle) and
//! `sha256(secret)`, never the secret, so a table dump yields no working
//! invite. Separate halves, rather than looking rows up by `sha256(token)`, so
//! the secret comparison is a real constant-time compare in Rust
//! (`pollis-delivery/src/groups.rs`) instead of a B-tree probe.
//!
//! Minted client-side: only the hash is posted at creation. The parse/hash half
//! is mirrored in `pollis-delivery` (`invite_token.rs`) with no shared code, so
//! a format change must land in both.

use base64::Engine as _;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Bytes of entropy in the public lookup handle: enough to make collisions
/// negligible.
const SELECTOR_BYTES: usize = 12;

/// Bytes of entropy in the secret half. 256 bits costs nothing: the token is
/// copied, never typed.
const SECRET_BYTES: usize = 32;

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// A freshly minted invite token, returned to the creator once and never
/// persisted.
#[derive(Debug, Clone)]
pub struct MintedToken {
    /// The full `<selector>.<secret>` string.
    pub token: String,
    /// Public lookup handle, stored verbatim server-side.
    pub selector: String,
    /// Lowercase hex `sha256(secret)`, stored server-side in place of the secret.
    pub secret_hash: String,
}

/// Mint a new invite token from OS entropy.
pub fn mint() -> MintedToken {
    let mut selector_bytes = [0u8; SELECTOR_BYTES];
    let mut secret_bytes = [0u8; SECRET_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut selector_bytes);
    rand::rngs::OsRng.fill_bytes(&mut secret_bytes);

    let selector = B64.encode(selector_bytes);
    let secret = B64.encode(secret_bytes);
    let secret_hash = hash_secret(&secret);

    MintedToken {
        token: format!("{selector}.{secret}"),
        selector,
        secret_hash,
    }
}

/// Lowercase hex `sha256` of the secret half.
///
/// Hashes the base64 text, not the decoded bytes, so the DS (which only sees
/// the string) reproduces it without a decode step.
pub fn hash_secret(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hex::encode(hasher.finalize())
}

/// Split a presented token into `(selector, secret)`.
///
/// `None` for anything malformed; callers must treat it exactly like a
/// non-matching token.
pub fn parse(token: &str) -> Option<(String, String)> {
    let trimmed = token.trim();
    let (selector, secret) = trimmed.split_once('.')?;
    if selector.is_empty() || secret.is_empty() {
        return None;
    }
    // A well-formedness check on attacker-supplied input, not a
    // secret-dependent branch.
    let ok = |s: &str| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok(selector) || !ok(secret) {
        return None;
    }
    Some((selector.to_string(), secret.to_string()))
}

/// Extract a token from either a bare code or a full invite URL.
///
/// Accepts `https://pollis.com/invite/<token>`, `pollis://invite/<token>`, or
/// the bare token.
pub fn extract(input: &str) -> Option<(String, String)> {
    let trimmed = input.trim();
    // Last path segment, minus any query or fragment a chat client appended.
    let candidate = trimmed
        .rsplit('/')
        .next()
        .unwrap_or(trimmed)
        .split(['?', '#'])
        .next()
        .unwrap_or(trimmed);
    parse(candidate)
}

/// Build the shareable URL for a token.
pub fn invite_url(token: &str) -> String {
    format!("https://pollis.com/invite/{token}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_token_round_trips_and_hides_the_secret() {
        let minted = mint();
        let (selector, secret) = parse(&minted.token).expect("minted token must parse");

        assert_eq!(selector, minted.selector);
        assert_eq!(hash_secret(&secret), minted.secret_hash);

        // The stored material must not contain the token.
        assert!(!minted.secret_hash.contains(&secret));
        assert!(!minted.token.contains(&minted.secret_hash));
        assert_ne!(minted.secret_hash, secret);
        assert_eq!(minted.secret_hash.len(), 64);
    }

    #[test]
    fn secret_half_carries_at_least_128_bits() {
        let minted = mint();
        let (_, secret) = parse(&minted.token).unwrap();
        let decoded = B64.decode(&secret).expect("secret must be base64url");
        assert!(
            decoded.len() * 8 >= 128,
            "secret must carry >=128 bits, got {}",
            decoded.len() * 8
        );
        assert_eq!(decoded.len(), SECRET_BYTES);
    }

    #[test]
    fn tokens_are_unique_across_many_mints() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1_000 {
            assert!(seen.insert(mint().token), "OsRng produced a duplicate token");
        }
    }

    // `pollis-delivery/src/invite_token.rs` pins the same vector.
    #[test]
    fn hash_secret_matches_the_pollis_delivery_vector() {
        assert_eq!(
            hash_secret("test-secret"),
            "9caf06bb4436cdbfa20af9121a626bc1093c4f54b31c0fa937957856135345b6"
        );
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        assert!(parse("").is_none());
        assert!(parse("nodot").is_none());
        assert!(parse(".onlysecret").is_none());
        assert!(parse("onlyselector.").is_none());
        // Outside the base64url alphabet.
        assert!(parse("abc.de f").is_none());
        assert!(parse("ab*c.def").is_none());
        assert!(parse("abc.def/gh").is_none());
    }

    #[test]
    fn extract_accepts_url_and_bare_forms() {
        let minted = mint();
        let expected = parse(&minted.token).unwrap();

        assert_eq!(extract(&minted.token).unwrap(), expected);
        assert_eq!(extract(&invite_url(&minted.token)).unwrap(), expected);
        assert_eq!(
            extract(&format!("pollis://invite/{}", minted.token)).unwrap(),
            expected
        );
        // Trailing query/fragment junk from a chat client.
        assert_eq!(
            extract(&format!("{}?utm=chat", invite_url(&minted.token))).unwrap(),
            expected
        );
        // Surrounding whitespace from a copy-paste.
        assert_eq!(
            extract(&format!("  {}  ", minted.token)).unwrap(),
            expected
        );
    }
}
