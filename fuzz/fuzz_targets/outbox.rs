//! Stateful fuzz target for the durable outbox's row transitions (#1335,
//! #1537): `enqueue_on`, a failed or refused attempt, `settle_on`, Discard and
//! the drain's listing, on a real local schema, against a plain model.
//!
//! Fuzzed: sends (and a renderer re-invoking a send under the same id), retries
//! the drain makes from its listing, attempts that land or fail, transient and
//! permanent refusals, discards, and process deaths at any point — a death is
//! simply the absence of the next transition, so every prefix of every
//! sequence is also checked.
//!
//! Properties, after every step:
//!   * **Never duplicated:** at most one row per message id, and re-enqueuing a
//!     delivered id reports it delivered instead of queueing it again.
//!   * **Never dropped:** every message accepted and not delivered, refused or
//!     discarded is in the drain's listing, and the listing holds nothing else,
//!     oldest first.
//!   * **Never demoted:** a delivered message stays delivered.
//!
//! The renderer half of the outbox (`pendingMessages.ts`, desktop and mobile)
//! is TypeScript, unreachable from here; its reducers are pinned by the two
//! `pending-messages.test.ts` suites.
//!
//! NEGATIVE CHECK (teeth): with `--cfg fuzz_mutant` a failed attempt is marked
//! with an unguarded `UPDATE`, so a late failure demotes a delivered message.
#![no_main]

use std::collections::BTreeMap;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use pollis_core::commands::messages::fuzz_surface::Outbox;

#[derive(Debug, Arbitrary)]
enum Op {
    Send(u8),
    Fail(u8, bool),
    Land(u8),
    Discard(u8),
    /// The drain lists what is unsent and tries the first; `true` = it lands.
    Drain(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Pending,
    Failed,
    Rejected,
    Delivered,
}

const IDS: u8 = 6;

fn id(i: u8) -> String {
    format!("01J0000000000000000000000{}", (b'A' + i % IDS) as char)
}

fn fail(outbox: &Outbox, id: &str, permanent: bool) {
    #[cfg(fuzz_mutant)]
    {
        // BUG: no `send_state IS NOT NULL` guard.
        let _ = permanent;
        outbox
            .conn()
            .execute("UPDATE message SET send_state = 'failed' WHERE id = ?1", [id])
            .expect("update");
    }
    #[cfg(not(fuzz_mutant))]
    outbox.fail(id, permanent).expect("fail");
}

fuzz_target!(|ops: Vec<Op>| {
    let outbox = Outbox::new().expect("outbox");
    // id -> (state, sent_at)
    let mut model: BTreeMap<String, (State, String)> = BTreeMap::new();
    let mut clock = 0u32;
    for op in ops.into_iter().take(64) {
        match op {
            Op::Send(i) => {
                let id = id(i);
                clock += 1;
                let at = format!("2026-10-28T00:{:02}:{:02}+00:00", clock / 60, clock % 60);
                let delivered = outbox
                    .enqueue(&id, "conv", "me", "hello", &at)
                    .expect("enqueue");
                match model.get(&id) {
                    Some((State::Delivered, _)) => assert!(delivered, "re-sent a delivered message"),
                    Some(_) => assert!(!delivered),
                    None => {
                        assert!(!delivered);
                        model.insert(id, (State::Pending, at));
                    }
                }
            }
            Op::Fail(i, permanent) => {
                let id = id(i);
                if !model.contains_key(&id) {
                    continue;
                }
                fail(&outbox, &id, permanent);
                if let Some((s, _)) = model.get_mut(&id) {
                    if *s != State::Delivered {
                        *s = if permanent { State::Rejected } else { State::Failed };
                    }
                }
            }
            Op::Land(i) => {
                let id = id(i);
                if let Some((s, _)) = model.get_mut(&id) {
                    outbox.settle(&id).expect("settle");
                    *s = State::Delivered;
                }
            }
            Op::Discard(i) => {
                let id = id(i);
                let removed = outbox.discard(&id).expect("discard");
                let unsent = matches!(
                    model.get(&id),
                    Some((State::Pending | State::Failed | State::Rejected, _))
                );
                assert_eq!(removed, unsent, "discard of {id}");
                if unsent {
                    model.remove(&id);
                }
            }
            Op::Drain(lands) => {
                let listed = outbox.unsent().expect("unsent");
                if let Some(first) = listed.first() {
                    if lands {
                        outbox.settle(first).expect("settle");
                        model.get_mut(first).expect("listed is modelled").0 = State::Delivered;
                    } else {
                        fail(&outbox, first, false);
                        model.get_mut(first).expect("listed is modelled").0 = State::Failed;
                    }
                }
            }
        }

        // The invariants, after every step (a death here leaves exactly this).
        let mut want: Vec<(&String, &String)> = model
            .iter()
            .filter(|(_, (s, _))| matches!(s, State::Pending | State::Failed))
            .map(|(id, (_, at))| (at, id))
            .collect();
        want.sort();
        let want: Vec<String> = want.into_iter().map(|(_, id)| id.clone()).collect();
        assert_eq!(outbox.unsent().expect("unsent"), want, "the drain would miss or repeat a message");
        for i in 0..IDS {
            let id = id(i);
            assert!(outbox.rows(&id).expect("rows") <= 1, "{id} stored twice");
            let stored = outbox.state(&id).expect("state");
            match model.get(&id) {
                None => assert_eq!(stored, None, "{id} should not exist"),
                Some((State::Delivered, _)) => {
                    assert_eq!(stored, Some(None), "delivered {id} was demoted")
                }
                Some((State::Pending, _)) => assert_eq!(stored, Some(Some("pending".into()))),
                Some((State::Failed, _)) => assert_eq!(stored, Some(Some("failed".into()))),
                Some((State::Rejected, _)) => assert_eq!(stored, Some(Some("rejected".into()))),
            }
        }
    }
});
