//! Fuzz target for message framing and padding (#1537); decrypted payloads are untrusted.
//!
//! Properties:
//!   * **No panic** on any buffer, through `classify`, `strip` and `thread_of`.
//!   * **No misparse:** a parsed redaction/receipt is exactly what its encoder writes for that value.
//!   * **Round trip:** every encoder's output classifies back to what went in:
//!     text (`pad`), threaded text (`pad_threaded`), a redaction, a receipt;
//!     `strip(pad(t)) == t`; padding never shrinks.
//!
//! Teeth: `--cfg fuzz_mutant` ignores the redaction length prefix.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use pollis_core::commands::messages::fuzz_surface::{
    classify, pad, pad_receipt, pad_redaction, pad_threaded, strip, thread_of, Frame, ReceiptKind,
};

#[derive(Debug, Arbitrary)]
enum Input {
    Raw(Vec<u8>),
    Text(Vec<u8>),
    Threaded(Vec<u8>, String),
    Redaction(String),
    Receipt(bool, String, Vec<String>),
}

const HEADER: usize = 5;

fn run_classify(buf: &[u8]) -> Frame {
    #[cfg(fuzz_mutant)]
    {
        if buf.first() == Some(&0xF6) && buf.len() >= HEADER {
            // BUG: ignores the length prefix.
            let id = String::from_utf8_lossy(&buf[HEADER..]);
            return Frame::Redaction(id.trim_end_matches('\0').to_string());
        }
    }
    classify(buf)
}

fn kind(read: bool) -> ReceiptKind {
    if read {
        ReceiptKind::Read
    } else {
        ReceiptKind::Delivered
    }
}

/// The encoded length before padding, for the misparse check.
fn receipt_len(at: &str, ids: &[String]) -> usize {
    2 + 4 + at.len() + 4 + ids.iter().map(|i| 4 + i.len()).sum::<usize>()
}

fuzz_target!(|input: Input| {
    match input {
        Input::Raw(buf) => {
            let _ = strip(&buf);
            let _ = thread_of(&buf);
            match run_classify(&buf) {
                Frame::Redaction(id) => {
                    let enc = pad_redaction(&id);
                    let n = HEADER + id.len();
                    assert!(
                        buf.len() >= n && buf[..n] == enc[..n],
                        "a buffer read as redaction {id:?} is not that redaction's encoding"
                    );
                }
                Frame::Receipt { kind, at, message_ids } => {
                    let enc = pad_receipt(kind, &at, &message_ids);
                    let n = receipt_len(&at, &message_ids);
                    assert!(
                        buf.len() >= n && buf[..n] == enc[..n],
                        "a buffer read as a receipt is not that receipt's encoding"
                    );
                }
                Frame::Text { .. } => {}
            }
        }
        Input::Text(t) => {
            let padded = pad(&t);
            assert!(padded.len() >= t.len() + HEADER, "padding shrank the message");
            assert_eq!(strip(&padded), t, "strip(pad(t)) != t");
            match run_classify(&padded) {
                Frame::Text { text, thread_id } => {
                    assert_eq!(text, t);
                    assert_eq!(thread_id, None);
                }
                _ => panic!("padded text classified as a control frame"),
            }
        }
        Input::Threaded(t, tid) => match run_classify(&pad_threaded(&t, &tid)) {
            Frame::Text { text, thread_id } => {
                assert_eq!(text, t);
                assert_eq!(thread_id.as_deref(), Some(tid.as_str()));
            }
            _ => panic!("a thread reply classified as a control frame"),
        },
        Input::Redaction(id) => match run_classify(&pad_redaction(&id)) {
            Frame::Redaction(got) => assert_eq!(got, id),
            _ => panic!("a redaction did not classify as one"),
        },
        Input::Receipt(read, at, ids) => match run_classify(&pad_receipt(kind(read), &at, &ids)) {
            Frame::Receipt { kind: k, at: a, message_ids } => {
                assert_eq!(k, kind(read));
                assert_eq!(a, at);
                assert_eq!(message_ids, ids);
            }
            _ => panic!("a receipt did not classify as one"),
        },
    }
});
