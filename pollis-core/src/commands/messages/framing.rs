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

/// First byte of the v1 **redaction control frame** ("delete for everyone").
/// A redaction is an ordinary MLS application message whose plaintext is
/// `[0xF6][u32 LE id-len][target message id]` zero-padded to a size bucket, so
/// on the wire (and in a Turso breach) a redaction is indistinguishable by
/// length from a short text message, and the server never learns which message
/// was redacted. Drawn from the same non-UTF-8 `0xF5..=0xFF` range as
/// [`PAD_FRAMING_V1`], so it can never collide with legacy unpadded text.
const REDACT_FRAMING_V1: u8 = 0xF6;

/// Marker for the optional **thread trailer** (#825), written immediately
/// after the real plaintext inside an otherwise ordinary [`PAD_FRAMING_V1`]
/// text frame:
///
/// ```text
///  byte 0        : PAD_FRAMING_V1 (0xF5)
///  bytes 1..5    : u32 LE real-plaintext length      <- unchanged
///  bytes 5..N    : real plaintext                    <- unchanged
///  byte N        : THREAD_TRAILER_V1 (0xF7)
///  bytes N+1..N+5: u32 LE thread-id length
///  bytes N+5..M  : thread id (ULID, UTF-8)
///  bytes M..     : zero padding up to the bucket size
/// ```
///
/// ## Why a trailer instead of a new frame version
///
/// A brand-new marker byte (`0xF8…`) would have been the obvious move, but an
/// OLD client hits the `strip` fallback on an unrecognised marker and returns
/// the whole buffer verbatim — so every thread reply would render as binary
/// mojibake on any client that predates this change. "Messages must work"
/// (CLAUDE.md) rules that out.
///
/// Hanging the thread id off the END of the existing v1 frame keeps the header
/// and the length prefix byte-for-byte what they always were, so an old
/// client's `strip` reads the same length prefix, returns exactly the same
/// plaintext, and never looks past it. A thread reply degrades to a perfectly
/// ordinary channel message on an old client — it loses the threading, not the
/// message.
///
/// The marker cannot be confused with padding (which is `0x00`) and cannot be
/// confused with the plaintext (which ended at `N`), so its presence at `N` is
/// unambiguous. It is drawn from the same reserved `0xF5..=0xFF` range as the
/// frame markers so the two namespaces never collide.
const THREAD_TRAILER_V1: u8 = 0xF7;

/// First byte of the v1 **receipt control frame** (#857, DMs only).
///
/// A delivery/read receipt is an ordinary MLS application message whose
/// plaintext is
///
/// ```text
///  byte 0          : RECEIPT_FRAMING_V1 (0xF8)
///  byte 1          : kind — 0 = delivered, 1 = read
///  bytes 2..6      : u32 LE  length of the RFC3339 timestamp
///  bytes 6..6+T    : timestamp (UTF-8)
///  next 4 bytes    : u32 LE  message-id count
///  then, per id    : u32 LE length + id bytes (ULID, UTF-8)
///  remainder       : zero padding up to the size bucket
/// ```
///
/// zero-padded to a size bucket exactly like every other frame, so on the wire
/// (and in a Turso breach) a receipt is indistinguishable **by length** from a
/// short text message and the server never learns who read what, or when. It
/// rides `/v1/messages/send` as an ordinary `type='message'` envelope for the
/// same reason: a dedicated `/v1/receipts/*` endpoint would put "this is a
/// receipt" in the request line, handing the untrusted DS precisely the metadata
/// this frame exists to withhold.
///
/// Receipts are **batched** — one frame carries every message id a reader is
/// acknowledging in that conversation — so a burst of catch-up ingest emits one
/// envelope, not one per message.
///
/// Drawn from the same non-UTF-8 `0xF5..=0xFF` range as the other markers, which
/// is what makes the back-compat degradation safe: a client that predates
/// receipts hits the `strip` fallback, gets the buffer verbatim, fails the
/// `String::from_utf8` check in the ingest path, and silently ignores the frame
/// instead of rendering mojibake. See `old_client_ignores_receipt_frame`.
const RECEIPT_FRAMING_V1: u8 = 0xF8;

/// Framing header: 1 version byte + 4-byte little-endian length prefix. Shared
/// by the padded-text ([`PAD_FRAMING_V1`]) and redaction ([`REDACT_FRAMING_V1`])
/// frames.
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

/// Which of the two receipt signals a [`Frame::Receipt`] carries (#857).
///
/// The two are deliberately **distinct** and never conflated:
/// - [`ReceiptKind::Delivered`] — the reader's device fetched the envelope and
///   successfully MLS-decrypted it. Emitted by the ingest path; says nothing
///   about a human.
/// - [`ReceiptKind::Read`] — a human actually saw the message: it was on screen,
///   in a focused window, in the conversation the user is looking at. Emitted
///   only by an explicit foreground call from the renderer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReceiptKind {
    Delivered,
    Read,
}

impl ReceiptKind {
    /// Wire byte. Stable — it is inside the MLS ciphertext, but old and new
    /// clients must still agree.
    fn to_byte(self) -> u8 {
        match self {
            ReceiptKind::Delivered => 0,
            ReceiptKind::Read => 1,
        }
    }

    fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(ReceiptKind::Delivered),
            1 => Some(ReceiptKind::Read),
            // An unknown kind from a future client is dropped rather than
            // guessed — recording it under the wrong kind would be worse than
            // not recording it.
            _ => None,
        }
    }

    /// The `message_receipt.kind` column value.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ReceiptKind::Delivered => "delivered",
            ReceiptKind::Read => "read",
        }
    }
}

/// A decrypted, de-framed message payload. Ordinary text (a text message, an
/// edit, or an attachment envelope) is [`Frame::Text`] carrying the exact
/// plaintext; a "delete for everyone" control message is
/// [`Frame::Redaction`] carrying the target message id; a delivery/read
/// acknowledgement is [`Frame::Receipt`] (#857).
pub(crate) enum Frame {
    Text {
        text: Vec<u8>,
        /// Set when the sender wrote a thread trailer (#825) — the ULID of the
        /// thread's root message. `None` for an ordinary channel message, and
        /// for every message sent by a client that predates threads.
        thread_id: Option<String>,
    },
    Redaction(String),
    Receipt {
        kind: ReceiptKind,
        /// The reader's own RFC3339 timestamp for the acknowledgement. Inside
        /// authenticated ciphertext, so only the reader could have written it.
        at: String,
        /// Every message id this frame acknowledges. Batched, so one envelope
        /// covers a whole catch-up burst.
        message_ids: Vec<String>,
    },
}

/// Wrap `target_message_id` in the v1 redaction framing and zero-pad it to its
/// size bucket. The returned buffer is handed to `try_mls_encrypt` exactly like
/// a text send, so a redaction is length-indistinguishable from a short message.
pub(crate) fn pad_redaction(target_message_id: &str) -> Vec<u8> {
    let id = target_message_id.as_bytes();
    let mut buf = Vec::with_capacity(HEADER + id.len());
    buf.push(REDACT_FRAMING_V1);
    buf.extend_from_slice(&(id.len() as u32).to_le_bytes());
    buf.extend_from_slice(id);
    let target = padded_len(buf.len());
    buf.resize(target, 0u8);
    buf
}

/// Wrap a batch of receipt acknowledgements in the v1 receipt framing and
/// zero-pad to the size bucket (#857). See [`RECEIPT_FRAMING_V1`] for the layout.
///
/// The buffer is handed to `try_mls_encrypt` exactly like a text send, so a
/// receipt is length-indistinguishable from a short message.
pub(crate) fn pad_receipt(kind: ReceiptKind, at: &str, message_ids: &[String]) -> Vec<u8> {
    let at_bytes = at.as_bytes();
    let mut buf = Vec::with_capacity(2 + 4 + at_bytes.len() + 4 + message_ids.len() * 30);
    buf.push(RECEIPT_FRAMING_V1);
    buf.push(kind.to_byte());
    buf.extend_from_slice(&(at_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(at_bytes);
    buf.extend_from_slice(&(message_ids.len() as u32).to_le_bytes());
    for id in message_ids {
        let id_bytes = id.as_bytes();
        buf.extend_from_slice(&(id_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(id_bytes);
    }
    let target = padded_len(buf.len());
    buf.resize(target, 0u8);
    buf
}

/// Read a u32 LE at `at`, returning the value and the offset just past it.
/// `None` when the four bytes are not fully inside `buf` — every caller is
/// parsing a buffer it did not build, so no read may be assumed in-bounds.
fn read_u32(buf: &[u8], at: usize) -> Option<(usize, usize)> {
    let end = at.checked_add(4)?;
    if end > buf.len() {
        return None;
    }
    let v = u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) as usize;
    Some((v, end))
}

/// Read a u32-LE-length-prefixed UTF-8 string at `at`, returning it and the
/// offset just past it. `None` on any overrun or invalid UTF-8.
fn read_str(buf: &[u8], at: usize) -> Option<(String, usize)> {
    let (len, after_len) = read_u32(buf, at)?;
    let end = after_len.checked_add(len)?;
    if end > buf.len() {
        return None;
    }
    let s = std::str::from_utf8(&buf[after_len..end]).ok()?.to_string();
    Some((s, end))
}

/// Parse a [`RECEIPT_FRAMING_V1`] buffer. `None` for any non-receipt or
/// malformed buffer, so the caller falls through to the ordinary text path.
fn parse_receipt(buf: &[u8]) -> Option<Frame> {
    if buf.first() != Some(&RECEIPT_FRAMING_V1) {
        return None;
    }
    let kind = ReceiptKind::from_byte(*buf.get(1)?)?;
    let (at, after_at) = read_str(buf, 2)?;
    let (count, mut pos) = read_u32(buf, after_at)?;
    // A hostile count must not pre-allocate gigabytes; the ids are read one at a
    // time and the loop is bounded by the buffer, so cap the reservation only.
    let mut message_ids = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let (id, next) = read_str(buf, pos)?;
        message_ids.push(id);
        pos = next;
    }
    Some(Frame::Receipt { kind, at, message_ids })
}

/// Classify a decrypted buffer. Keys on the first byte:
///
/// - `0xF6` ([`REDACT_FRAMING_V1`]) → [`Frame::Redaction`] with the target id.
/// - `0xF8` ([`RECEIPT_FRAMING_V1`]) → [`Frame::Receipt`] (#857).
/// - anything else — v1 padded text (`0xF5`), legacy unpadded UTF-8, or an
///   attachment envelope (`{`) → [`Frame::Text`] via [`strip`].
///
/// A malformed redaction or receipt frame (too short, bad length prefix,
/// non-UTF-8 payload) degrades to `Text` — it cannot arise from
/// [`pad_redaction`] / [`pad_receipt`] and exists only as belt-and-braces so a
/// hostile buffer can never panic the ingest path. Such a buffer then fails the
/// ingest path's `String::from_utf8` check (its lead byte is not valid UTF-8)
/// and is dropped rather than stored as a visible message.
pub(crate) fn classify(buf: &[u8]) -> Frame {
    if let Some(receipt) = parse_receipt(buf) {
        return receipt;
    }
    if buf.first() == Some(&REDACT_FRAMING_V1) && buf.len() >= HEADER {
        let len = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
        let end = HEADER + len;
        if end <= buf.len() {
            if let Ok(id) = std::str::from_utf8(&buf[HEADER..end]) {
                return Frame::Redaction(id.to_string());
            }
        }
    }
    Frame::Text {
        text: strip(buf),
        thread_id: thread_of(buf),
    }
}

/// Wrap `plaintext` in the v1 framing with a thread trailer naming
/// `thread_id`, then zero-pad to the size bucket (#825).
///
/// Identical to [`pad`] up to the end of the plaintext — which is exactly what
/// makes a thread reply readable on a client that has never heard of threads.
pub(crate) fn pad_threaded(plaintext: &[u8], thread_id: &str) -> Vec<u8> {
    let id = thread_id.as_bytes();
    let mut buf = Vec::with_capacity(HEADER + plaintext.len() + HEADER + id.len());
    buf.push(PAD_FRAMING_V1);
    buf.extend_from_slice(&(plaintext.len() as u32).to_le_bytes());
    buf.extend_from_slice(plaintext);
    buf.push(THREAD_TRAILER_V1);
    buf.extend_from_slice(&(id.len() as u32).to_le_bytes());
    buf.extend_from_slice(id);
    let target = padded_len(buf.len());
    buf.resize(target, 0u8);
    buf
}

/// Recover the thread id from a decrypted buffer, if the sender wrote one.
///
/// Returns `None` for every buffer [`pad`] produces, for legacy unpadded
/// messages, for attachment envelopes, and for any malformed trailer — a
/// message that merely loses its threading is far better than one that fails
/// to decode, so every ambiguous case degrades to "not in a thread".
pub(crate) fn thread_of(buf: &[u8]) -> Option<String> {
    if buf.first() != Some(&PAD_FRAMING_V1) || buf.len() < HEADER {
        return None;
    }
    let len = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    // `checked_add` because `len` is attacker-controlled in the sense that it
    // arrives from a decrypted buffer we did not build; an overflow here would
    // wrap and index somewhere arbitrary.
    let end = HEADER.checked_add(len)?;
    // `>=`, not `>`: the trailer marker itself must be within the buffer.
    if end >= buf.len() || buf[end] != THREAD_TRAILER_V1 {
        return None;
    }
    let id_len_at = end.checked_add(1)?;
    let id_at = id_len_at.checked_add(4)?;
    if id_at > buf.len() {
        return None;
    }
    let id_len = u32::from_le_bytes([
        buf[id_len_at],
        buf[id_len_at + 1],
        buf[id_len_at + 2],
        buf[id_len_at + 3],
    ]) as usize;
    let id_end = id_at.checked_add(id_len)?;
    if id_end > buf.len() {
        return None;
    }
    std::str::from_utf8(&buf[id_at..id_end])
        .ok()
        .map(str::to_string)
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

    /// `pad_redaction` -> `classify` recovers the exact target id, for the ULID
    /// shape used in production and a few edge lengths.
    #[test]
    fn redaction_roundtrip_recovers_target_id() {
        for id in [
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "",
            "x",
            &"9".repeat(64),
        ] {
            let framed = pad_redaction(id);
            match classify(&framed) {
                Frame::Redaction(got) => assert_eq!(got, id, "redaction id must round-trip"),
                _ => panic!("a redaction frame must classify as Redaction (id={id:?})"),
            }
        }
    }

    /// A redaction frame is padded to the SAME size bucket as an ordinary short
    /// text message, so its length leaks nothing: the server cannot tell a
    /// redaction apart from a normal message by envelope size.
    #[test]
    fn redaction_is_length_indistinguishable_from_text() {
        let redaction = pad_redaction("01ARZ3NDEKTSV4RRFFQ69G5FAV");
        let short_text = pad(b"hey");
        assert_eq!(redaction.len(), MIN_BUCKET);
        assert_eq!(redaction.len(), short_text.len());
    }

    /// Text, legacy/unpadded, and attachment payloads all classify as `Text`
    /// with their plaintext intact — only a `0xF6` lead byte means redaction.
    #[test]
    fn non_redaction_payloads_classify_as_text() {
        let cases: &[&[u8]] = &[
            b"hello world",
            &pad(b"hello world"),
            br#"{"_att":[{"hash":"abc","key":"k"}]}"#,
            b"",
        ];
        for &buf in cases {
            match classify(buf) {
                Frame::Text { text, .. } => assert_eq!(text, strip(buf)),
                _ => panic!("non-redaction payload misclassified: {buf:?}"),
            }
        }
    }

    /// A truncated / malformed redaction frame degrades to `Text` rather than
    /// panicking — the ingest path must never trust attacker-controllable bytes.
    #[test]
    fn malformed_redaction_frame_degrades_to_text() {
        // Lead byte says redaction but the length prefix overruns the buffer.
        let mut bad = vec![REDACT_FRAMING_V1];
        bad.extend_from_slice(&999u32.to_le_bytes());
        bad.extend_from_slice(b"short");
        assert!(matches!(classify(&bad), Frame::Text { .. }));
    }

    // ── Thread trailer (#825) ───────────────────────────────────────────────

    const THREAD: &str = "01J8XQ2M5H7NR3T0V9WZ4KDCEB";

    /// THE load-bearing back-compat test. A client that predates threads only
    /// ever calls `strip`; if the trailer disturbed the header or the length
    /// prefix, every thread reply would render as mojibake on an old client.
    /// `strip` must return the plaintext byte-for-byte, exactly as it does for
    /// an unthreaded `pad`.
    #[test]
    fn old_client_strip_recovers_thread_reply_verbatim() {
        for text in [
            "".as_bytes(),
            "ok".as_bytes(),
            "a longer reply that crosses a bucket boundary ".repeat(20).as_bytes(),
            // Multi-byte UTF-8 must survive untouched.
            "héllo → 🧵".as_bytes(),
        ] {
            let framed = pad_threaded(text, THREAD);
            assert_eq!(
                strip(&framed),
                text,
                "a threads-unaware client must recover the plaintext exactly",
            );
        }
    }

    #[test]
    fn pad_threaded_roundtrips_thread_id() {
        let framed = pad_threaded(b"in a thread", THREAD);
        assert_eq!(thread_of(&framed).as_deref(), Some(THREAD));
        match classify(&framed) {
            Frame::Text { text, thread_id } => {
                assert_eq!(text, b"in a thread");
                assert_eq!(thread_id.as_deref(), Some(THREAD));
            }
            _ => panic!("threaded text misclassified as redaction"),
        }
    }

    /// An ordinary send carries no trailer, so it must never be mistaken for a
    /// thread reply — including when the plaintext itself ends in the trailer
    /// marker byte or in zeros.
    #[test]
    fn unthreaded_payloads_have_no_thread_id() {
        assert_eq!(thread_of(&pad(b"plain message")), None);
        assert_eq!(thread_of(&pad(b"")), None);
        assert_eq!(thread_of(&pad(&[0x00, 0x00, 0x00])), None);
        assert_eq!(thread_of(&pad(&[THREAD_TRAILER_V1])), None);
        // Legacy unpadded text and attachment envelopes.
        assert_eq!(thread_of(b"legacy unpadded"), None);
        assert_eq!(thread_of(br#"{"_att":[]}"#), None);
        assert_eq!(thread_of(&[]), None);
        // A redaction frame is not a text frame and has no thread.
        assert_eq!(thread_of(&pad_redaction("01JABC")), None);
    }

    /// A truncated or hostile trailer degrades to "no thread" instead of
    /// panicking. These cannot arise from `pad_threaded`; the ingest path must
    /// survive them anyway, since the buffer is only as trustworthy as the
    /// sender.
    #[test]
    fn malformed_thread_trailer_degrades_to_none() {
        let good = pad_threaded(b"hi", THREAD);
        let text_end = HEADER + 2;

        // Trailer marker present but the id length prefix is truncated away.
        let truncated = good[..text_end + 1].to_vec();
        assert_eq!(thread_of(&truncated), None);

        // Id length prefix overruns the buffer.
        let mut overrun = good[..text_end + 1].to_vec();
        overrun.extend_from_slice(&9_999u32.to_le_bytes());
        overrun.extend_from_slice(b"short");
        assert_eq!(thread_of(&overrun), None);

        // Length prefix large enough that `HEADER + len` would overflow.
        let mut overflow = vec![PAD_FRAMING_V1];
        overflow.extend_from_slice(&u32::MAX.to_le_bytes());
        overflow.extend_from_slice(b"body");
        assert_eq!(thread_of(&overflow), None);

        // Non-UTF-8 thread id.
        let mut bad_utf8 = good[..text_end + 1].to_vec();
        bad_utf8.extend_from_slice(&2u32.to_le_bytes());
        bad_utf8.extend_from_slice(&[0xFF, 0xFE]);
        assert_eq!(thread_of(&bad_utf8), None);
    }

    // ── Receipt frame (#857) ────────────────────────────────────────────────

    const MSG_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MSG_B: &str = "01J8XQ2M5H7NR3T0V9WZ4KDCEB";

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// `pad_receipt` -> `classify` recovers the kind, timestamp and the full id
    /// batch, for both kinds and for batch sizes from empty to large.
    #[test]
    fn receipt_roundtrips_kind_time_and_batch() {
        let at = "2026-08-15T12:34:56.789Z";
        let batches = vec![
            ids(&[]),
            ids(&[MSG_A]),
            ids(&[MSG_A, MSG_B]),
            (0..200).map(|i| format!("01ARZ3NDEKTSV4RRFFQ69G5F{i:03}")).collect(),
        ];
        for kind in [ReceiptKind::Delivered, ReceiptKind::Read] {
            for batch in &batches {
                let framed = pad_receipt(kind, at, batch);
                match classify(&framed) {
                    Frame::Receipt { kind: got_kind, at: got_at, message_ids } => {
                        assert_eq!(got_kind, kind, "kind must round-trip");
                        assert_eq!(got_at, at, "timestamp must round-trip");
                        assert_eq!(&message_ids, batch, "the whole batch must round-trip");
                    }
                    _ => panic!("a receipt frame must classify as Receipt (kind={kind:?})"),
                }
            }
        }
    }

    /// Delivered and Read are DISTINCT signals on the wire — the whole point of
    /// not conflating "your device got it" with "a human saw it". A frame built
    /// as one must never decode as the other.
    #[test]
    fn delivered_and_read_are_distinct_on_the_wire() {
        let d = pad_receipt(ReceiptKind::Delivered, "2026-08-15T00:00:00Z", &ids(&[MSG_A]));
        let r = pad_receipt(ReceiptKind::Read, "2026-08-15T00:00:00Z", &ids(&[MSG_A]));
        assert_ne!(d, r, "the two kinds must not produce identical bytes");
        match (classify(&d), classify(&r)) {
            (Frame::Receipt { kind: a, .. }, Frame::Receipt { kind: b, .. }) => {
                assert_eq!(a, ReceiptKind::Delivered);
                assert_eq!(b, ReceiptKind::Read);
            }
            _ => panic!("both must classify as receipts"),
        }
    }

    /// A receipt is padded to the SAME size bucket as a short text message, so
    /// the DS/Turso cannot pick receipts out of the envelope stream by length.
    /// This is the property that keeps "who read what, when" off the server.
    #[test]
    fn receipt_is_length_indistinguishable_from_text() {
        let receipt = pad_receipt(ReceiptKind::Read, "2026-08-15T12:34:56Z", &ids(&[MSG_A]));
        assert_eq!(receipt.len(), MIN_BUCKET);
        assert_eq!(receipt.len(), pad(b"hey").len());
        assert_eq!(receipt.len(), pad_redaction(MSG_A).len());
        // A realistic multi-message catch-up batch still collapses to the floor.
        let batch = pad_receipt(ReceiptKind::Delivered, "2026-08-15T12:34:56Z", &ids(&[MSG_A, MSG_B]));
        assert_eq!(batch.len(), MIN_BUCKET);
        // And a big batch is still bucketed, never a bespoke length.
        let big: Vec<String> = (0..50).map(|i| format!("01ARZ3NDEKTSV4RRFFQ69G5F{i:03}")).collect();
        let big_framed = pad_receipt(ReceiptKind::Read, "2026-08-15T12:34:56Z", &big);
        assert_eq!(big_framed.len(), padded_len(big_framed.len()));
    }

    /// THE back-compat test. A client that predates receipts only ever calls
    /// `strip`, which returns a `0xF8` buffer verbatim; the ingest path then
    /// tries `String::from_utf8` on it. `0xF8` can never begin valid UTF-8, so
    /// the frame is silently ignored — never rendered as a garbage message.
    #[test]
    fn old_client_ignores_receipt_frame() {
        for kind in [ReceiptKind::Delivered, ReceiptKind::Read] {
            let framed = pad_receipt(kind, "2026-08-15T00:00:00Z", &ids(&[MSG_A, MSG_B]));
            assert_eq!(strip(&framed), framed, "an old client's strip returns it verbatim");
            assert!(
                String::from_utf8(strip(&framed)).is_err(),
                "the verbatim buffer must fail UTF-8, so ingest drops it instead of rendering it",
            );
            assert_eq!(thread_of(&framed), None, "a receipt is not a threaded text frame");
        }
    }

    /// Text, redaction and legacy payloads must never be mistaken for receipts,
    /// and a receipt must never be mistaken for text or a redaction.
    #[test]
    fn receipts_do_not_collide_with_other_frames() {
        let non_receipts: Vec<Vec<u8>> = vec![
            pad(b"hello"),
            pad_threaded(b"hello", THREAD),
            pad_redaction(MSG_A),
            b"legacy unpadded".to_vec(),
            br#"{"_att":[{"hash":"a","key":"k"}]}"#.to_vec(),
            Vec::new(),
        ];
        for buf in &non_receipts {
            assert!(
                !matches!(classify(buf), Frame::Receipt { .. }),
                "non-receipt payload misclassified as a receipt: {buf:?}",
            );
        }
        let receipt = pad_receipt(ReceiptKind::Read, "2026-08-15T00:00:00Z", &ids(&[MSG_A]));
        assert!(matches!(classify(&receipt), Frame::Receipt { .. }));
    }

    /// A truncated or hostile receipt frame degrades to `Text` (and is then
    /// dropped by ingest's UTF-8 check) rather than panicking. These cannot
    /// arise from `pad_receipt`; the ingest path must survive them anyway,
    /// because the buffer is only as trustworthy as the sender.
    #[test]
    fn malformed_receipt_frame_degrades_to_text() {
        let cases: Vec<Vec<u8>> = vec![
            // Lead byte only — no kind byte.
            vec![RECEIPT_FRAMING_V1],
            // Unknown kind byte from a future client.
            {
                let mut b = vec![RECEIPT_FRAMING_V1, 99];
                b.extend_from_slice(&1u32.to_le_bytes());
                b.push(b'x');
                b.extend_from_slice(&0u32.to_le_bytes());
                b
            },
            // Timestamp length prefix overruns the buffer.
            {
                let mut b = vec![RECEIPT_FRAMING_V1, 0];
                b.extend_from_slice(&9_999u32.to_le_bytes());
                b.extend_from_slice(b"short");
                b
            },
            // Well-formed timestamp, but the id count promises more than exists.
            {
                let mut b = vec![RECEIPT_FRAMING_V1, 1];
                b.extend_from_slice(&1u32.to_le_bytes());
                b.push(b't');
                b.extend_from_slice(&5u32.to_le_bytes());
                b
            },
            // Count would overflow the reservation arithmetic.
            {
                let mut b = vec![RECEIPT_FRAMING_V1, 0];
                b.extend_from_slice(&0u32.to_le_bytes());
                b.extend_from_slice(&u32::MAX.to_le_bytes());
                b
            },
            // Non-UTF-8 timestamp.
            {
                let mut b = vec![RECEIPT_FRAMING_V1, 0];
                b.extend_from_slice(&2u32.to_le_bytes());
                b.extend_from_slice(&[0xFF, 0xFE]);
                b.extend_from_slice(&0u32.to_le_bytes());
                b
            },
        ];
        for buf in &cases {
            assert!(
                matches!(classify(buf), Frame::Text { .. }),
                "malformed receipt must degrade to Text, not panic or parse: {buf:?}",
            );
        }
    }

    /// The trailer must not defeat the padding it sits inside — a thread reply
    /// is still bucketed, so its ciphertext length leaks no more than an
    /// ordinary short message's does.
    #[test]
    fn threaded_payloads_are_still_bucketed() {
        for text in ["a", "a much longer thread reply", ""] {
            let framed = pad_threaded(text.as_bytes(), THREAD);
            assert_eq!(
                framed.len(),
                padded_len(framed.len()),
                "threaded payload escaped its size bucket",
            );
            assert_eq!(framed.len(), MIN_BUCKET, "short replies must collapse");
        }
    }
}
