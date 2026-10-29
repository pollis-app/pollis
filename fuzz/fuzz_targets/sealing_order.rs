//! Track-B fuzz target for the catch-up envelope list and the decrypt order
//! (#1537): `ingest::partition_envelopes` and `ingest::into_sealing_order`,
//! through `pollis_core::commands::messages::fuzz_surface`.
//!
//! Every channel of a group shares one MLS group, so one sender's ratchet runs
//! across all of them, and a device that catches up must decrypt each epoch's
//! run in the order the DS accepted the envelopes (the 2026-10-27 loss, #1526).
//!
//! Two inputs:
//!   * **Model:** envelopes inserted into several conversations in some
//!     interleaving, numbered the way the DS numbers them (a per-conversation
//!     `seq` and a group-wide `arrival`, both assigned in the inserting
//!     transaction), delivered in the DS's `(conversation, seq)` listing order.
//!     Properties: each listed conversation's bucket holds exactly its
//!     envelopes in `seq` order and nothing else; an unlisted one is dropped;
//!     the sorted run is the insertion order (every sender's order kept); each
//!     conversation's own order is untouched; with no arrival (an older DS)
//!     the run is left exactly as listed.
//!   * **Raw:** arbitrary bytes as a `CatchUpResponse` — no panic, and every
//!     envelope lands in the bucket of its own conversation or nowhere.
//!
//! NEGATIVE CHECK (teeth): with `--cfg fuzz_mutant` the run is left in listing
//! order — the conversation-by-conversation decrypt #1527 replaced.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use pollis_api::directory::{CatchUpResponse, EnvelopeWire};
use pollis_core::commands::messages::fuzz_surface::{into_sealing_order, partition_envelopes};

#[derive(Debug, Arbitrary)]
enum Input {
    Model {
        listed: u8,
        /// Conversation of each inserted envelope, in insertion order. A value
        /// past `listed` is a conversation the device does not list.
        inserts: Vec<u8>,
        old_ds: bool,
    },
    Raw(Vec<u8>),
}

fn run_order(run: &mut [(usize, usize)], arrival: &[Vec<Option<i64>>]) {
    #[cfg(not(fuzz_mutant))]
    into_sealing_order(run, arrival);
    #[cfg(fuzz_mutant)]
    {
        // BUG: decrypt conversation by conversation.
        let _ = (run, arrival);
    }
}

fn wire(cid: &str, id: String, seq: i64, arrival: Option<i64>) -> EnvelopeWire {
    EnvelopeWire {
        conversation_id: cid.to_string(),
        id,
        sender_id: "sealed".to_string(),
        ciphertext: "mls:00".to_string(),
        reply_to_id: None,
        target_message_id: None,
        sent_at: "2026-10-27T00:00:00+00:00".to_string(),
        kind: "message".to_string(),
        seq: Some(seq),
        arrival,
    }
}

fuzz_target!(|input: Input| {
    match input {
        Input::Model {
            listed,
            inserts,
            old_ds,
        } => model(listed, inserts, old_ds),
        Input::Raw(bytes) => raw(&bytes),
    }
});

fn model(listed: u8, inserts: Vec<u8>, old_ds: bool) {
    let listed = 1 + (listed % 4) as usize;
    let inserts: Vec<usize> = inserts.into_iter().take(40).map(|c| (c % 5) as usize).collect();
    let cid = |c: usize| format!("c{c}");
    // Number them as the DS does, in the inserting transaction.
    let mut next_seq = [0i64; 5];
    let mut rows: Vec<(usize, i64, i64)> = Vec::new();
    for (arrival, &c) in inserts.iter().enumerate() {
        next_seq[c] += 1;
        rows.push((c, next_seq[c], arrival as i64));
    }
    // ...and list them as the DS lists them: by conversation, then seq.
    let mut listing = rows.clone();
    listing.sort_by_key(|&(c, seq, _)| (cid(c), seq));
    let envelopes: Vec<EnvelopeWire> = listing
        .iter()
        .map(|&(c, seq, a)| wire(&cid(c), format!("e{a}"), seq, (!old_ds).then_some(a)))
        .collect();
    let ids: Vec<String> = (0..listed).map(cid).collect();
    let (per_conv, arrival) = partition_envelopes(ids.clone(), envelopes);

    assert_eq!(per_conv.len(), listed);
    for (ci, (got_cid, envs)) in per_conv.iter().enumerate() {
        assert_eq!(got_cid, &ids[ci], "buckets out of listing order");
        let want: Vec<(String, i64)> = listing
            .iter()
            .filter(|&&(c, _, _)| c == ci)
            .map(|&(_, seq, a)| (format!("e{a}"), seq))
            .collect();
        assert_eq!(envs, &want, "conversation {ci} holds the wrong envelopes");
        assert_eq!(arrival[ci].len(), envs.len(), "arrival shape differs");
    }

    let mut run: Vec<(usize, usize)> = per_conv
        .iter()
        .enumerate()
        .flat_map(|(ci, (_, envs))| (0..envs.len()).map(move |ei| (ci, ei)))
        .collect();
    let listed_order = run.clone();
    run_order(&mut run, &arrival);
    if old_ds {
        assert_eq!(run, listed_order, "an older DS's run was reordered");
        return;
    }
    let order: Vec<i64> = run
        .iter()
        .map(|&(ci, ei)| arrival[ci][ei].expect("arrival"))
        .collect();
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(order, sorted, "the run is not in the order the senders sealed");
    for ci in 0..listed {
        let mine: Vec<usize> = run.iter().filter(|r| r.0 == ci).map(|r| r.1).collect();
        assert!(mine.windows(2).all(|w| w[0] < w[1]), "conversation {ci} reordered");
    }
}

fn raw(bytes: &[u8]) {
    let Ok(resp) = serde_json::from_slice::<CatchUpResponse>(bytes) else {
        return;
    };
    let ids = resp.conversation_ids.clone();
    let sent = resp.envelopes.clone();
    let (per_conv, arrival) = partition_envelopes(resp.conversation_ids, resp.envelopes);
    assert_eq!(per_conv.len(), ids.len());
    let mut placed = 0usize;
    for (ci, (cid, envs)) in per_conv.iter().enumerate() {
        assert_eq!(cid, &ids[ci]);
        assert_eq!(arrival[ci].len(), envs.len());
        for (id, _) in envs {
            assert!(
                sent.iter().any(|e| &e.id == id && &e.conversation_id == cid),
                "envelope {id} filed under a conversation it is not in"
            );
        }
        placed += envs.len();
    }
    let listed = sent.iter().filter(|e| ids.contains(&e.conversation_id)).count();
    assert_eq!(placed, listed, "a listed conversation's envelope was dropped or doubled");
    let mut run: Vec<(usize, usize)> = per_conv
        .iter()
        .enumerate()
        .flat_map(|(ci, (_, envs))| (0..envs.len()).map(move |ei| (ci, ei)))
        .collect();
    let before = run.len();
    run_order(&mut run, &arrival);
    assert_eq!(run.len(), before);
}
