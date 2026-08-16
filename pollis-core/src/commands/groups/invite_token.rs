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
//! **The selector is public; the secret is not.** The server stores the selector
//! verbatim (it is the indexed lookup handle) and `sha256(secret)` — never the
//! secret. That split is the whole point of the format:
//!
//! * Storing only a hash means a dump of `group_invite_link` yields no working
//!   invite. Recovering one means inverting SHA-256 over 256 bits of OS entropy.
//! * Keeping a separate plaintext selector means redemption is still a single
//!   indexed probe. The obvious alternative — look the row up by
//!   `sha256(token)` — also stores only a hash, but then the *database index*
//!   performs the secret comparison, and a B-tree probe is neither constant-time
//!   nor auditable from Rust. Splitting the halves is what lets the comparison
//!   be a real constant-time compare (`pollis-delivery/src/groups.rs`) instead of
//!   a nominal one.
//!
//! The token is generated **client-side**: this module mints it, hashes the
//! secret locally, and only the hash is posted to the Delivery Service. The
//! server therefore never sees a usable token at creation time — only at
//! redemption, where it must, and where it stores nothing.
//!
//! The parse/hash half of this file is mirrored in `pollis-delivery`
//! (`invite_token.rs`). The two crates share no code by design — the same
//! arrangement the repo uses for the device-cert wire format and for
//! account-creation-from-email (`auth::resolve_or_create_user_by_email` ↔
//! `pollis_delivery::otp::apply_verify_otp`) — so any change to the format must
//! land in both.

use base64::Engine as _;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Bytes of entropy in the public lookup handle. Not a secret; it only needs to
/// make accidental selector collisions negligible.
const SELECTOR_BYTES: usize = 12;

/// Bytes of entropy in the secret half. 256 bits — comfortably past the 128-bit
/// floor, and free: the token is copied, never typed.
const SECRET_BYTES: usize = 32;

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// A freshly minted invite token. The `token` field is the only place the secret
/// ever exists in full; it is returned to the creator once and never persisted.
#[derive(Debug, Clone)]
pub struct MintedToken {
    /// The full `<selector>.<secret>` string. Show once, store never.
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
/// Hashes the base64 TEXT of the secret rather than its decoded bytes so that
/// the DS — which only ever sees the string form — can reproduce this without
/// a decode step that could itself fail or normalise differently.
pub fn hash_secret(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hex::encode(hasher.finalize())
}

/// Split a presented token into `(selector, secret)`.
///
/// Returns `None` for anything malformed. Callers must treat `None` exactly like
/// a non-matching token — see the note on indistinguishable failures in
/// `redeem_group_invite_link`.
pub fn parse(token: &str) -> Option<(String, String)> {
    let trimmed = token.trim();
    let (selector, secret) = trimmed.split_once('.')?;
    if selector.is_empty() || secret.is_empty() {
        return None;
    }
    // Reject anything outside the base64url alphabet up front. This is a
    // well-formedness check on ATTACKER-SUPPLIED input, not a secret-dependent
    // branch: it tells an attacker only that their own input was malformed,
    // which they already know.
    let ok = |s: &str| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok(selector) || !ok(secret) {
        return None;
    }
    Some((selector.to_string(), secret.to_string()))
}

/// Extract a token from either a bare code or a full invite URL.
///
/// People paste whatever they were sent — `https://pollis.com/invite/<token>`,
/// the in-app `pollis://invite/<token>`, or the bare token. Accepting all three
/// is the difference between "not overly complicated to use" and a support
/// burden, and it costs nothing: the token is validated identically either way.
pub fn extract(input: &str) -> Option<(String, String)> {
    let trimmed = input.trim();
    // Take the last path segment for URL-shaped input, dropping any query or
    // fragment a chat client may have appended.
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

        // The stored material must NOT contain the token. This is the property
        // the issue asks for: a database read yields no working invite.
        assert!(!minted.secret_hash.contains(&secret));
        assert!(!minted.token.contains(&minted.secret_hash));
        assert_ne!(minted.secret_hash, secret);
        // sha256 hex is always 64 chars.
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

    // The DS must reproduce this byte-for-byte (`pollis-delivery/src/invite_token.rs`
    // pins the SAME vector). If either side's `hash_secret` changes, both tests
    // fail and the mirrored format has drifted.
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
