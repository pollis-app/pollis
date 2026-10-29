//! Size padding and control frames inside the MLS plaintext, so ciphertext
//! length does not leak message length (#331, `docs/metadata-minimization-design.md`
//! §4.1, `.codesight/wiki/mls.md`).
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
//! ## Back-compat invariant
//!
//! Every frame marker is in `0xF5..=0xFF`, which never begins valid UTF-8. Text
//! plaintext is always UTF-8 and attachment envelopes (left unpadded, §4.1) are
//! JSON starting with `{`, so [`strip`] returns any unframed buffer verbatim
//! without ambiguity.

/// First byte of the v1 padded text framing.
const PAD_FRAMING_V1: u8 = 0xF5;

/// First byte of the v1 redaction ("delete for everyone") frame:
/// `[0xF6][u32 LE id-len][target message id]`, padded like text so the server
/// cannot tell a redaction from a short message.
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
/// A trailer rather than a new frame version: an old client's `strip` reads the
/// unchanged length prefix and shows the reply as an ordinary message, where a
/// new marker would render as mojibake.
const THREAD_TRAILER_V1: u8 = 0xF7;

/// First byte of the v1 delivery/read receipt frame (#857, DMs only):
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
/// Padded like text and sent as an ordinary message envelope, so the DS never
/// learns who read what. An old client's ingest fails `String::from_utf8` on it
/// and drops it.
const RECEIPT_FRAMING_V1: u8 = 0xF8;

/// 1 version byte + u32 LE length prefix (text and redaction frames).
const HEADER: usize = 1 + 4;

/// Smallest padded length: all short messages share one observable size.
const MIN_BUCKET: usize = 256;

/// Round `n` up to its size bucket: [`MIN_BUCKET`] below it, PADMÉ above it (a
/// multiple of `2^(E - S)`, `E = floor(log2 n)`, `S` = bits of `E`; at most ~12%
/// overhead).
fn padded_len(n: usize) -> usize {
    if n <= MIN_BUCKET {
        return MIN_BUCKET;
    }
    let floor_log2 = |x: usize| (usize::BITS - 1 - x.leading_zeros()) as usize;
    let e = floor_log2(n);
    let s = floor_log2(e) + 1;
    // n > 256 => e >= 8 > s, so the bucket is at least 2.
    let last_bits = e - s;
    let bucket = 1usize << last_bits;
    (n + bucket - 1) & !(bucket - 1)
}

/// Wrap a text plaintext in the v1 framing, padded to its bucket. Never used for
/// attachment envelopes.
pub fn pad(plaintext: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(HEADER + plaintext.len());
    buf.push(PAD_FRAMING_V1);
    buf.extend_from_slice(&(plaintext.len() as u32).to_le_bytes());
    buf.extend_from_slice(plaintext);
    let target = padded_len(buf.len());
    buf.resize(target, 0u8);
    buf
}

/// Recover the real plaintext from a decrypted buffer; unframed or malformed
/// buffers are returned verbatim.
pub fn strip(buf: &[u8]) -> Vec<u8> {
    if buf.first() != Some(&PAD_FRAMING_V1) {
        return buf.to_vec();
    }
    if buf.len() < HEADER {
        return buf.to_vec();
    }
    let len = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    // Checked: on 32-bit armeabi-v7a `HEADER + u32::MAX` wraps and the slice
    // would panic every member's ingest (#1537).
    let Some(end) = HEADER.checked_add(len) else {
        return buf.to_vec();
    };
    if end > buf.len() {
        return buf.to_vec();
    }
    buf[HEADER..end].to_vec()
}

/// Receipt signal (#857): `Delivered` = the device decrypted it (ingest);
/// `Read` = a human saw it on screen (explicit renderer call). Never conflated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReceiptKind {
    Delivered,
    Read,
}

impl ReceiptKind {
    /// Wire byte; stable across client versions.
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
            // A future kind is dropped, not guessed.
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

/// A decrypted, de-framed payload: text (message, edit or attachment envelope),
/// a redaction's target id, or a receipt batch.
pub enum Frame {
    Text {
        text: Vec<u8>,
        /// Thread root ULID from the trailer (#825), if any.
        thread_id: Option<String>,
    },
    Redaction(String),
    Receipt {
        kind: ReceiptKind,
        /// The reader's RFC3339 timestamp, authenticated by MLS.
        at: String,
        /// Batched so a catch-up burst is one envelope.
        message_ids: Vec<String>,
    },
}

/// Wrap `target_message_id` in the v1 redaction framing, padded to its bucket.
pub fn pad_redaction(target_message_id: &str) -> Vec<u8> {
    let id = target_message_id.as_bytes();
    let mut buf = Vec::with_capacity(HEADER + id.len());
    buf.push(REDACT_FRAMING_V1);
    buf.extend_from_slice(&(id.len() as u32).to_le_bytes());
    buf.extend_from_slice(id);
    let target = padded_len(buf.len());
    buf.resize(target, 0u8);
    buf
}

/// Wrap a receipt batch in the v1 receipt framing ([`RECEIPT_FRAMING_V1`]),
/// padded to its bucket.
pub fn pad_receipt(kind: ReceiptKind, at: &str, message_ids: &[String]) -> Vec<u8> {
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

/// Bounds-checked u32 LE read; returns the value and the next offset.
fn read_u32(buf: &[u8], at: usize) -> Option<(usize, usize)> {
    let end = at.checked_add(4)?;
    if end > buf.len() {
        return None;
    }
    let v = u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) as usize;
    Some((v, end))
}

/// Bounds-checked length-prefixed UTF-8 string read.
fn read_str(buf: &[u8], at: usize) -> Option<(String, usize)> {
    let (len, after_len) = read_u32(buf, at)?;
    let end = after_len.checked_add(len)?;
    if end > buf.len() {
        return None;
    }
    let s = std::str::from_utf8(&buf[after_len..end]).ok()?.to_string();
    Some((s, end))
}

/// `None` for any non-receipt or malformed buffer.
fn parse_receipt(buf: &[u8]) -> Option<Frame> {
    if buf.first() != Some(&RECEIPT_FRAMING_V1) {
        return None;
    }
    let kind = ReceiptKind::from_byte(*buf.get(1)?)?;
    let (at, after_at) = read_str(buf, 2)?;
    let (count, mut pos) = read_u32(buf, after_at)?;
    // Cap the reservation: a hostile count must not pre-allocate gigabytes.
    let mut message_ids = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let (id, next) = read_str(buf, pos)?;
        message_ids.push(id);
        pos = next;
    }
    Some(Frame::Receipt { kind, at, message_ids })
}

/// Classify a decrypted buffer by its first byte. A malformed redaction or
/// receipt degrades to `Text`, which ingest then drops on its UTF-8 check, so a
/// hostile buffer can never panic ingest.
pub fn classify(buf: &[u8]) -> Frame {
    if let Some(receipt) = parse_receipt(buf) {
        return receipt;
    }
    if buf.first() == Some(&REDACT_FRAMING_V1) && buf.len() >= HEADER {
        let len = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
        // Same 32-bit wrap as `strip` (#1537).
        let end = HEADER.saturating_add(len);
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

/// [`pad`] plus a thread trailer naming `thread_id` (#825); identical to `pad`
/// up to the end of the plaintext.
pub fn pad_threaded(plaintext: &[u8], thread_id: &str) -> Vec<u8> {
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

/// The thread id from a trailer, if present. Every ambiguous or malformed case
/// degrades to `None`: losing the threading beats losing the message.
pub fn thread_of(buf: &[u8]) -> Option<String> {
    if buf.first() != Some(&PAD_FRAMING_V1) || buf.len() < HEADER {
        return None;
    }
    let len = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
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

    /// The through-MLS round trip lives in `mls/tests.rs`.
    #[test]
    fn pad_strip_roundtrip_exact() {
        let mut sizes = vec![0usize, 1, 2, 10, 100, 200, 250, 251, 252, 255, 256, 257];
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

    #[test]
    fn distinct_sizes_collapse_to_one_bucket() {
        let a = pad(b"");
        let b = pad(b"ok");
        let c = pad(&[b'x'; 100]);
        let d = pad(&vec![b'y'; MIN_BUCKET - HEADER]);
        assert_eq!(a.len(), MIN_BUCKET);
        assert_eq!(a.len(), b.len());
        assert_eq!(a.len(), c.len());
        assert_eq!(a.len(), d.len(), "the largest that still fits the floor bucket");

        // Framed lengths 260 and 270 both fall in the (256, 272] band.
        let big1 = pad(&vec![b'a'; 255]);
        let big2 = pad(&vec![b'b'; 265]);
        assert_eq!(big1.len(), 272);
        assert_eq!(
            big1.len(),
            big2.len(),
            "255 and 265 bytes should land in the same bucket"
        );

        let distinct_outputs: std::collections::BTreeSet<usize> =
            (0..=240).map(|n| pad(&vec![0u8; n]).len()).collect();
        assert_eq!(distinct_outputs.len(), 1, "0..=240 bytes all share one bucket");
    }

    #[test]
    fn legacy_unpadded_message_passes_through() {
        for legacy in [
            &b""[..],
            &b"hello world"[..],
            "unicode: \u{1F600} \u{00E9} \u{4E2D}\u{6587}".as_bytes(),
            br#"{"_att":[{"hash":"abc","key":"k"}]}"#,
        ] {
            assert_eq!(strip(legacy), legacy, "legacy/unframed bytes must be returned verbatim");
        }
    }

    #[test]
    fn framing_marker_is_not_valid_utf8() {
        assert!(std::str::from_utf8(&[PAD_FRAMING_V1]).is_err());
        assert_eq!(pad(b"hi")[0], PAD_FRAMING_V1);
    }

    /// The length prefix, not a sentinel, delimits the real bytes.
    #[test]
    fn plaintext_with_trailing_zeros_roundtrips() {
        let original = vec![0u8; 300];
        let padded = pad(&original);
        assert_eq!(strip(&padded), original);
    }

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

    #[test]
    fn redaction_is_length_indistinguishable_from_text() {
        let redaction = pad_redaction("01ARZ3NDEKTSV4RRFFQ69G5FAV");
        let short_text = pad(b"hey");
        assert_eq!(redaction.len(), MIN_BUCKET);
        assert_eq!(redaction.len(), short_text.len());
    }

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

    #[test]
    fn malformed_redaction_frame_degrades_to_text() {
        let mut bad = vec![REDACT_FRAMING_V1];
        bad.extend_from_slice(&999u32.to_le_bytes());
        bad.extend_from_slice(b"short");
        assert!(matches!(classify(&bad), Frame::Text { .. }));
    }

    const THREAD: &str = "01J8XQ2M5H7NR3T0V9WZ4KDCEB";

    /// A client that predates threads only calls `strip`.
    #[test]
    fn old_client_strip_recovers_thread_reply_verbatim() {
        for text in [
            "".as_bytes(),
            "ok".as_bytes(),
            "a longer reply that crosses a bucket boundary ".repeat(20).as_bytes(),
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

    #[test]
    fn unthreaded_payloads_have_no_thread_id() {
        assert_eq!(thread_of(&pad(b"plain message")), None);
        assert_eq!(thread_of(&pad(b"")), None);
        assert_eq!(thread_of(&pad(&[0x00, 0x00, 0x00])), None);
        assert_eq!(thread_of(&pad(&[THREAD_TRAILER_V1])), None);
        assert_eq!(thread_of(b"legacy unpadded"), None);
        assert_eq!(thread_of(br#"{"_att":[]}"#), None);
        assert_eq!(thread_of(&[]), None);
        assert_eq!(thread_of(&pad_redaction("01JABC")), None);
    }

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
        // ...and neither `strip` nor a redaction frame trusts it (#1537).
        assert_eq!(strip(&overflow), overflow);
        let mut redaction = vec![REDACT_FRAMING_V1];
        redaction.extend_from_slice(&u32::MAX.to_le_bytes());
        redaction.extend_from_slice(b"body");
        assert!(matches!(classify(&redaction), Frame::Text { .. }));

        // Non-UTF-8 thread id.
        let mut bad_utf8 = good[..text_end + 1].to_vec();
        bad_utf8.extend_from_slice(&2u32.to_le_bytes());
        bad_utf8.extend_from_slice(&[0xFF, 0xFE]);
        assert_eq!(thread_of(&bad_utf8), None);
    }

    const MSG_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MSG_B: &str = "01J8XQ2M5H7NR3T0V9WZ4KDCEB";

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

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

    #[test]
    fn receipt_is_length_indistinguishable_from_text() {
        let receipt = pad_receipt(ReceiptKind::Read, "2026-08-15T12:34:56Z", &ids(&[MSG_A]));
        assert_eq!(receipt.len(), MIN_BUCKET);
        assert_eq!(receipt.len(), pad(b"hey").len());
        assert_eq!(receipt.len(), pad_redaction(MSG_A).len());
        let batch = pad_receipt(ReceiptKind::Delivered, "2026-08-15T12:34:56Z", &ids(&[MSG_A, MSG_B]));
        assert_eq!(batch.len(), MIN_BUCKET);
        let big: Vec<String> = (0..50).map(|i| format!("01ARZ3NDEKTSV4RRFFQ69G5F{i:03}")).collect();
        let big_framed = pad_receipt(ReceiptKind::Read, "2026-08-15T12:34:56Z", &big);
        assert_eq!(big_framed.len(), padded_len(big_framed.len()));
    }

    /// A client that predates receipts gets the buffer verbatim from `strip` and
    /// drops it on the UTF-8 check.
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
