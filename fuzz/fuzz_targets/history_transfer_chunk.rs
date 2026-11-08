//! Fuzz target for the history-transfer receiver (#1735): untrusted sealed
//! chunks into `open`, untrusted plaintext into `parse_chunk`, untrusted rows
//! into `check_row`.
//!
//! Properties:
//!   * **No panic**, whatever the bytes.
//!   * **No forged acceptance:** a mutated genuine sealed chunk either fails to
//!     open or is byte-identical to the genuine one (same plaintext, same final flag).
//!   * **A parsed chunk is where the receiver stands:** right transfer, right
//!     index, the expected chain hash, a final flag that matches the body.
//!   * **An accepted row keeps the row rules:** a ULID id, never the placeholder
//!     author, exactly one of content / delete, `received_at` normalized and not
//!     in the future, `sent_at` byte-identical.
//!
//! Teeth: `--cfg fuzz_mutant` parses without checking the chain hash.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use pollis_core::commands::history_transfer::fuzz_surface as ht;

const KEY: [u8; 32] = [0x17; 32];
const TRANSFER: &str = "01J0000000000000000000TRNS";

#[derive(Arbitrary, Debug)]
struct Row {
    id: String,
    sender: String,
    content: Option<String>,
    sent_at: String,
    received_at: String,
    edited_at: Option<String>,
    deleted_at: Option<String>,
    ulid_id: bool,
}

#[derive(Arbitrary, Debug)]
struct Input {
    flips: Vec<(u16, u8)>,
    cut: Option<u16>,
    genuine_final: bool,
    raw_plain: Vec<u8>,
    expect_index: u8,
    expect_final: bool,
    rows: Vec<Row>,
}

fn parse(plain: &[u8], index: u32, prev: &[u8; 32], is_final: bool) -> Result<ht::ChunkPlain, ht::FrameError> {
    #[cfg(not(fuzz_mutant))]
    {
        ht::parse_chunk(plain, TRANSFER, index, prev, is_final)
    }
    #[cfg(fuzz_mutant)]
    {
        // The mutant: whatever chain hash the chunk claims is believed.
        let chunk: ht::ChunkPlain = serde_json::from_slice(plain).map_err(|_| ht::FrameError::Parse)?;
        let fake_prev = <[u8; 32]>::try_from(
            (0..32).map(|i| u8::from_str_radix(chunk.prev_hash.get(i * 2..i * 2 + 2).unwrap_or("00"), 16).unwrap_or(0)).collect::<Vec<_>>(),
        )
        .unwrap();
        let _ = prev;
        ht::parse_chunk(plain, TRANSFER, index, &fake_prev, is_final)
    }
}

fuzz_target!(|input: Input| {
    let now = chrono::Utc::now();
    let prev = ht::sha256(b"the previous chunk");

    // 1. Mutated genuine sealed chunks never open as anything else.
    let genuine_plain = serde_json::to_vec(&ht::ChunkPlain {
        v: ht::VERSION,
        transfer_id: TRANSFER.into(),
        index: 3,
        prev_hash: hex_of(&prev),
        rows: vec![],
        receipts: vec![],
        fin: None,
        estimated_total: None,
    })
    .unwrap();
    let sealed = ht::seal(&KEY, TRANSFER, 3, input.genuine_final, &genuine_plain);
    let mut tampered = sealed.clone();
    for (pos, x) in &input.flips {
        if !tampered.is_empty() {
            let i = *pos as usize % tampered.len();
            tampered[i] ^= x;
        }
    }
    if let Some(cut) = input.cut {
        tampered.truncate(cut as usize % (tampered.len() + 1));
    }
    if let Ok((plain, fin)) = ht::open(&KEY, TRANSFER, 3, &tampered) {
        assert_eq!(plain, genuine_plain, "a tampered chunk opened to other bytes");
        assert_eq!(fin, input.genuine_final, "the final flag changed under the AEAD");
    }

    // 2. Arbitrary plaintext, and arbitrary rows inside a well-formed chunk.
    let index = u32::from(input.expect_index % 4);
    let mut candidates = vec![input.raw_plain.clone()];
    let rows: Vec<ht::WireRow> = input
        .rows
        .iter()
        .take(64)
        .map(|r| ht::WireRow {
            id: if r.ulid_id { "01J0000000000000000000ROW1".into() } else { r.id.clone() },
            conversation_id: "c".into(),
            sender_id: r.sender.clone(),
            content: r.content.clone(),
            reply_to_id: None,
            thread_id: None,
            sent_at: r.sent_at.clone(),
            received_at: r.received_at.clone(),
            edited_at: r.edited_at.clone(),
            deleted_at: r.deleted_at.clone(),
        })
        .collect();
    candidates.push(
        serde_json::to_vec(&ht::ChunkPlain {
            v: ht::VERSION,
            transfer_id: TRANSFER.into(),
            index,
            prev_hash: if input.flips.is_empty() { hex_of(&prev) } else { hex_of(&[1u8; 32]) },
            rows,
            receipts: vec![],
            fin: None,
            estimated_total: None,
        })
        .unwrap(),
    );
    for plain in candidates {
        let Ok(chunk) = parse(&plain, index, &prev, input.expect_final) else {
            continue;
        };
        assert_eq!(chunk.transfer_id, TRANSFER);
        assert_eq!(chunk.index, index);
        assert_eq!(chunk.prev_hash, hex_of(&prev), "a chunk off the chain was accepted");
        assert_eq!(chunk.fin.is_some(), input.expect_final);
        for row in &chunk.rows {
            if let Ok(checked) = ht::check_row(row, now) {
                assert!(checked.row.id.len() == 26, "non-ULID id accepted: {:?}", checked.row.id);
                assert_ne!(checked.row.sender_id, ht::SEALED_SENDER);
                assert!(checked.row.content.is_some() != checked.row.deleted_at.is_some());
                assert_eq!(checked.row.sent_at, row.sent_at);
                assert!(checked.received_at.len() == 19);
                assert!(checked.received_at <= now.format("%Y-%m-%d %H:%M:%S").to_string());
            }
        }
    }
});

fn hex_of(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
