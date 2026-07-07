//! Text-message size padding (issue #331 v2, `docs/metadata-minimization-design.md` §4.1).
//!
//! MLS application ciphertext length tracks plaintext length, so anyone reading
//! `message_envelope` (Turso, a breach, a subpoena) learns the approximate
//! message length — enough to tell "ok" from a paragraph, fingerprint forwarded
//! content, or correlate a send with a receive by size. We defeat that by
//! padding the plaintext to a small set of fixed size **buckets** BEFORE
//! `try_mls_encrypt`, and stripping the padding right after `try_mls_decrypt`.
//! The framing lives entirely INSIDE the MLS ciphertext, so only members ever
//! see it and there is no schema or server change.
//!
//! ## Framing layout (version 1)
//!
//! ```text
//!  byte 0      : PAD_FRAMING_V1  (0xF5)         — framing version / marker
//!  bytes 1..5  : u32 LE          real-plaintext length
//!  bytes 5..N  : real plaintext  (N = 5 + len)
//!  bytes N..   : zero padding     up to the bucket size
//! ```
//!
//! ## Version-byte back-compat (the load-bearing invariant)
//!
//! A reader that understands the framing strips it; an OLD, unpadded message
//! must still decode byte-for-byte. Both are handled by [`strip`] keying on the
//! first byte:
//!
//! - The real plaintext of a text message / edit is ALWAYS a Rust `String`, i.e.
//!   valid UTF-8. The bytes `0xF5..=0xFF` can never begin (or even appear in) a
//!   valid UTF-8 string, so a legacy unpadded message can never be mistaken for
//!   framed. `strip` returns such bytes verbatim.
//! - Attachment envelopes are deliberately left UNPADDED (their R2 blob size is
//!   inherent and dedup depends on it, §4.1). Their plaintext is JSON beginning
//!   with `{` (0x7B), likewise never `0xF5`, so `strip` returns them verbatim
//!   too.
//!
//! Reserving the whole `0xF5..=0xFF` range for framing versions means a future
//! v2 framing (say `0xF6`) stays just as unambiguous against legacy UTF-8.

/// First byte of the v1 padded framing. Chosen from the range of bytes that can
/// never begin a valid UTF-8 string (`0xF5..=0xFF`), so a legacy unpadded
/// message — always valid UTF-8 — is never mistaken for framed. See the module
/// docs for the back-compat argument.
const PAD_FRAMING_V1: u8 = 0xF5;

/// Framing header: 1 version byte + 4-byte little-endian length prefix.
const HEADER: usize = 1 + 4;

/// Smallest padded plaintext length. Every message at or below this (empty,
/// "ok", a single emoji, a short reply) collapses to one observable size, so the
/// server cannot distinguish among the huge population of short messages.
const MIN_BUCKET: usize = 256;

/// Round `n` up to its size bucket.
///
/// Below [`MIN_BUCKET`] everything collapses to `MIN_BUCKET`. Above it we use
/// **PADMÉ** (Nym, "Reducing Metadata Leakage from Encrypted Files and
/// Communication with PURBs"): round `n` up to a multiple of `2^(E - S)`, where
/// `E = floor(log2 n)` and `S` is the number of bits needed to represent `E`.
/// This keeps the worst-case padding overhead to ~12% while still collapsing
/// many distinct lengths into each bucket. The result is always `>= n`.
fn padded_len(n: usize) -> usize {
    if n <= MIN_BUCKET {
        return MIN_BUCKET;
    }
    // floor(log2 x) for x >= 1.
    let floor_log2 = |x: usize| (usize::BITS - 1 - x.leading_zeros()) as usize;
    // n > MIN_BUCKET >= 1, so both logs below are well-defined.
    let e = floor_log2(n); // E = floor(log2 n)
    let s = floor_log2(e) + 1; // bits needed to represent E
    // n > 256 => e >= 8 > s, so last_bits >= 1 and bucket is a real power of two.
    let last_bits = e - s;
    let bucket = 1usize << last_bits;
    // Round n up to the next multiple of the (power-of-two) bucket size.
    (n + bucket - 1) & !(bucket - 1)
}

/// Wrap `plaintext` in the v1 framing and zero-pad it to its size bucket.
///
/// The returned buffer is what gets handed to `try_mls_encrypt` for a TEXT
/// message. [`strip`] recovers `plaintext` exactly. Callers must NOT pad
/// attachment envelopes (see the module docs).
pub(crate) fn pad(plaintext: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(HEADER + plaintext.len());
    buf.push(PAD_FRAMING_V1);
    // Text messages are far under 4 GiB; a u32 length prefix is plenty.
    buf.extend_from_slice(&(plaintext.len() as u32).to_le_bytes());
    buf.extend_from_slice(plaintext);
    let target = padded_len(buf.len());
    buf.resize(target, 0u8);
    buf
}

/// Recover the real plaintext from a decrypted buffer.
///
/// - **Framed (v1):** strips the header + zero padding and returns the exact
///   original bytes.
/// - **Legacy / unpadded** (old client, or an attachment envelope): the first
///   byte is not the framing marker, so the buffer is returned verbatim. This is
///   the version-byte back-compat that lets old and new clients interoperate.
///
/// The malformed-framing fallbacks (too short, or a length prefix past the end)
/// also return the buffer verbatim; they cannot fire for a buffer produced by
/// [`pad`] and exist only as defensive belt-and-braces.
pub(crate) fn strip(buf: &[u8]) -> Vec<u8> {
    if buf.first() != Some(&PAD_FRAMING_V1) {
        return buf.to_vec();
    }
    if buf.len() < HEADER {
        return buf.to_vec();
    }
    let len = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    let end = HEADER + len;
    if end > buf.len() {
        return buf.to_vec();
    }
    buf[HEADER..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// pad -> strip is the identity for a spread of sizes: empty, 1 byte, at and
    /// just over each bucket boundary, and large. (The MLS encrypt/decrypt layer
    /// is transparent to the payload, so this is the whole round-trip minus the
    /// crypto; the through-MLS version lives in `mls/tests.rs`.)
    #[test]
    fn pad_strip_roundtrip_exact() {
        let mut sizes = vec![0usize, 1, 2, 10, 100, 200, 250, 251, 252, 255, 256, 257];
        // Around the first PADMÉ bucket boundaries and well beyond.
        sizes.extend([271, 272, 273, 500, 1000, 1024, 4096, 16384, 16385, 100_000]);
        for &n in &sizes {
            // Vary the byte pattern by index so a stray zero-fill bug can't pass.
            let original: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
            let padded = pad(&original);
            assert!(
                padded.len() >= original.len() + HEADER,
                "padded ({}) must hold header + plaintext ({})",
                padded.len(),
                original.len()
            );
            let recovered = strip(&padded);
            assert_eq!(recovered, original, "round-trip must be byte-identical (n={n})");
        }
    }

    /// Several DISTINCT plaintext sizes must collapse to the SAME padded size —
    /// this is the whole point of bucketing.
    #[test]
    fn distinct_sizes_collapse_to_one_bucket() {
        // Everything up to MIN_BUCKET - HEADER collapses to MIN_BUCKET.
        let a = pad(b"");
        let b = pad(b"ok");
        let c = pad(&vec![b'x'; 100]);
        let d = pad(&vec![b'y'; MIN_BUCKET - HEADER]);
        assert_eq!(a.len(), MIN_BUCKET);
        assert_eq!(a.len(), b.len());
        assert_eq!(a.len(), c.len());
        assert_eq!(a.len(), d.len(), "the largest that still fits the floor bucket");

        // Two different larger lengths that share a PADMÉ bucket also collapse.
        // Framed lengths 260 and 270 both fall in the (256, 272] band -> 272.
        let big1 = pad(&vec![b'a'; 255]);
        let big2 = pad(&vec![b'b'; 265]);
        assert_eq!(big1.len(), 272);
        assert_eq!(
            big1.len(),
            big2.len(),
            "255 and 265 bytes should land in the same bucket"
        );

        // And the buckets are genuinely coarse: many inputs, few output sizes.
        let distinct_outputs: std::collections::BTreeSet<usize> =
            (0..=240).map(|n| pad(&vec![0u8; n]).len()).collect();
        assert_eq!(distinct_outputs.len(), 1, "0..=240 bytes all share one bucket");
    }

    /// Version-byte back-compat: an OLD, unpadded message (raw UTF-8 text, no
    /// framing) must survive `strip` byte-for-byte. This is what lets a
    /// pre-#331 send be read by a new client.
    #[test]
    fn legacy_unpadded_message_passes_through() {
        for legacy in [
            &b""[..],
            &b"hello world"[..],
            "unicode: \u{1F600} \u{00E9} \u{4E2D}\u{6587}".as_bytes(),
            // Attachment envelope shape: JSON beginning with '{' (0x7B).
            br#"{"_att":[{"hash":"abc","key":"k"}]}"#,
        ] {
            assert_eq!(strip(legacy), legacy, "legacy/unframed bytes must be returned verbatim");
        }
    }

    /// The framing marker can never collide with valid UTF-8, so no real text
    /// message ever looks framed to a legacy-only reader (and vice versa).
    #[test]
    fn framing_marker_is_not_valid_utf8() {
        assert!(std::str::from_utf8(&[PAD_FRAMING_V1]).is_err());
        // A padded buffer always leads with the marker; its raw form is not UTF-8.
        assert_eq!(pad(b"hi")[0], PAD_FRAMING_V1);
    }

    /// The zero padding is genuine trailing filler, not part of the plaintext:
    /// a plaintext that itself ends in zero bytes must still round-trip exactly
    /// (the length prefix, not a sentinel, delimits the real bytes).
    #[test]
    fn plaintext_with_trailing_zeros_roundtrips() {
        let original = vec![0u8; 300];
        let padded = pad(&original);
        assert_eq!(strip(&padded), original);
    }
}
